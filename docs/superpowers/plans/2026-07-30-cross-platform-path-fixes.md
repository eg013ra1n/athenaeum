# Cross-Platform Path Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix every Critical + Important finding of the 2026-07-30 cross-platform path audit (`docs/superpowers/research/2026-07-30-cross-platform-path-audit.md`) so file management, scanning, relinking, archive/restore, calibration-library outputs and export behave correctly on Windows (NTFS) and Linux (ext4).

**Architecture:** Three recurring fixes applied site-by-site: (1) replace the last unescaped `LIKE root || '%'` prefix predicates with the codebase-standard separator-strict byte-range helper; (2) apply the existing `normalize_path` (verbatim-`\\?\` strip) at every boundary where a canonicalized path escapes or is compared; (3) make path composition/decomposition component-based instead of string-based where `/`-separated zip paths meet `\`-separated OS paths. Plus targeted Windows-semantics fixes (case-only rename, EXDEV fallback, sharing-violation retry, longPathAware manifest, reversible header-identity encoding).

**Tech Stack:** Rust (rusqlite, walkdir, tauri-build 2.6), React/TS. No new dependencies.

## Global Constraints

- **Two backends in sync**: any change to a Tauri command gets the identical change in its Axum mirror in the same task (CLAUDE.md rule).
- **Never swallow errors**: every new failure path logs via `tracing` before returning.
- Logging style: message = short stable phrase, data in snake_case fields (`warn!(path = %p, error = %e, "…")`).
- Gates per repo convention: `cargo build --workspace`, `cargo test -p athenaeum-core` (workspace at the end), `npx tsc --noEmit`. clippy is NOT a gate. Format touched files with `rustfmt <file>` (never `cargo fmt -p`).
- Commit as the user (git config already set), one commit per task, conventional prefix `fix(xplat): …` / `docs: …`.
- Branch: continue on `0.5.1` (same unreleased version as the Folders redesign).
- Tests must be runnable on the macOS dev host — Windows-shaped path fixtures are strings in SQLite (the codebase's established pattern, see `overview_tests` in `api/scan_roots.rs:2740`); `#[cfg(windows)]`-only tests are allowed but never the sole coverage for a fix.
- The audit doc is the source for finding IDs referenced below (C1–C4, I1–I24).

### Shared fixture patterns (used by several tasks)

Files-table insert (from `api/scan_roots.rs:2756`):

```rust
conn.execute(
    "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, '2026-01-01T00:00:00Z', 'FITS')",
    rusqlite::params![path, filename, size],
).unwrap();
```

`fits_header` insert: `INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, 'H', ?2)`.
Test context: `ServiceContext::new_for_tests(tmp.path().join("catalog.db"))`; no-op progress emitter: `crate::events::NullEmitter`.

---

### Task 1: Relinking — byte-range predicate, filename sync, UTF-8 reject, walk depth cap

Fixes C1 (sibling-root sweep via `LIKE`), I11 (lossy path writes), I12 (unbounded symlink walk), plus the audit's Minor "relink updates `path` without `filename`".

**Files:**
- Modify: `crates/athenaeum-core/src/relinking/mod.rs:53-61` (predicate), `:91-95` (WalkDir), `:133-141` (UPDATE), `:191-199` (verify predicate)
- Modify: `crates/athenaeum-core/src/scanner/mod.rs:28` (`path_to_utf8` visibility)
- Test: `crates/athenaeum-core/src/relinking/mod.rs` (existing `tests` module)

**Interfaces:**
- Consumes: `crate::db::scan_root_prefix_predicate(column, roots) -> (String, Vec<rusqlite::types::Value>)` (`db/operations.rs:94`, `pub(crate)`, reachable as `crate::db::scan_root_prefix_predicate`); `crate::scanner::path_to_utf8` (made `pub(crate)` in this task).
- Produces: no signature changes; `relink_files` now also updates `files.filename`.

- [ ] **Step 1: Write the failing tests** (append to the `tests` module in `relinking/mod.rs`):

```rust
#[test]
fn relink_does_not_sweep_sibling_or_case_variant_roots() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();

    // Real tiny FITS in the NEW location so the walk yields a fingerprint.
    let dir = tempfile::tempdir().unwrap();
    let new_root = dir.path().join("relocated");
    std::fs::create_dir_all(&new_root).unwrap();
    let walked = new_root.join("x.fits");
    crate::fits_writer::write_fits_f32(&walked, 4, 4, 1, &vec![0.0f32; 16], &[]).unwrap();
    let header = extract_fits_header(&walked).unwrap();
    let fp = compute_header_fingerprint(&header);

    // Catalog rows under a NAME-PREFIX SIBLING root and a CASE-VARIANT root of
    // old root "/data/M31" — both must be invisible to the relink of /data/M31.
    let mut sibling_ids = Vec::new();
    for path in ["/data/M31_Ha/x.fits", "/data/m31/x.fits"] {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'x.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
            rusqlite::params![path],
        ).unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, 'H', ?2)",
            rusqlite::params![id, fp],
        ).unwrap();
        sibling_ids.push((id, path.to_string()));
    }

    let res = relink_files(&conn, "/data/M31", new_root.to_str().unwrap()).unwrap();
    assert_eq!(res.files_matched, 0, "sibling fingerprints must not enter the map");
    assert_eq!(res.files_new, 1);
    for (id, original) in &sibling_ids {
        let path: String = conn
            .query_row("SELECT path FROM files WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        assert_eq!(&path, original, "sibling row must be untouched");
    }
}

#[test]
fn relink_updates_path_and_filename_for_a_real_match() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let new_root = dir.path().join("relocated");
    std::fs::create_dir_all(&new_root).unwrap();
    let walked = new_root.join("renamed_on_disk.fits");
    crate::fits_writer::write_fits_f32(&walked, 4, 4, 1, &vec![0.0f32; 16], &[]).unwrap();
    let fp = compute_header_fingerprint(&extract_fits_header(&walked).unwrap());

    conn.execute(
        "INSERT INTO files (path, filename, size, modified_at, format) VALUES ('/data/M31/orig.fits', 'orig.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
        [],
    ).unwrap();
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, 'H', ?2)",
        rusqlite::params![id, fp],
    ).unwrap();

    let res = relink_files(&conn, "/data/M31", new_root.to_str().unwrap()).unwrap();
    assert_eq!(res.files_matched, 1);
    let (path, filename): (String, String) = conn
        .query_row("SELECT path, filename FROM files WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(path, walked.to_string_lossy());
    assert_eq!(filename, "renamed_on_disk.fits", "filename must follow the path");
}
```

Add the needed imports to the test module: `use crate::fingerprint::compute_header_fingerprint; use crate::fits_parser::extract_fits_header;` (top-level `use` at `relinking/mod.rs:8-9` already imports them for the main body — reuse via `super::*`).

- [ ] **Step 2: Run to verify both fail**

Run: `cargo test -p athenaeum-core relinking -- --nocapture`
Expected: `relink_does_not_sweep…` FAILS (sibling path rewritten / files_matched == 1); `relink_updates_path_and_filename…` FAILS on the filename assert.

- [ ] **Step 3: Implement**

3a. `scanner/mod.rs:28`: `fn path_to_utf8` → `pub(crate) fn path_to_utf8`.

3b. `relinking/mod.rs` — replace the fingerprint-map query (both in `relink_files` and `verify_files_at_location`, identical shape):

```rust
    // Separator-strict byte-range prefix (same helper as every destructive
    // root-scoped site since 81aedae7): a name-prefix sibling root
    // (/data/M31_Ha), a case-variant root (/data/m31 — LIKE is ASCII
    // case-insensitive), or a `_`/`%` in the root name can no longer pull
    // foreign rows into the fingerprint map and get their paths rewritten.
    let (pred, values) = crate::db::scan_root_prefix_predicate("f.path", &[old_root_path.to_string()]);
    let sql = format!(
        "SELECT f.id, f.path, f.filename, fh.header_fingerprint
         FROM files f
         INNER JOIN fits_header fh ON f.id = fh.file_id
         WHERE ({pred}) AND fh.header_fingerprint IS NOT NULL"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values.iter()), |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
```

(in `verify_files_at_location` the variable is `root_path`; delete the now-unused `old_root_prefix` / `root_prefix` lines.)

3c. WalkDir cap (`relinking/mod.rs:91`): add `.max_depth(64)` after `.follow_links(true)` with the same one-line comment the scanner uses (`scanner/mod.rs:174-179` rationale: walkdir loop detection isn't bulletproof on every filesystem).

3d. Match-update block (`:133-141`) — reject non-UTF-8 instead of lossy, and sync `filename`:

```rust
        if let Some((file_id, old_path)) = fingerprint_map.get(&fingerprint) {
            // Same invariant as the scanner (path_to_utf8): a U+FFFD-mangled
            // path would break every later exact/prefix lookup and std::fs open.
            let new_path_str = match crate::scanner::path_to_utf8(path) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping non-UTF-8 path during relink");
                    continue;
                }
            };
            let new_filename = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| new_path_str.clone());

            conn.execute(
                "UPDATE files SET path = ?1, filename = ?2 WHERE id = ?3",
                params![new_path_str, new_filename, file_id],
            )
            .context("Failed to update file path")?;
```

(the `file_name()` lossy is safe here — the full path already passed `path_to_utf8`.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p athenaeum-core relinking`
Expected: PASS (all, including the two pre-existing empty-list tests).

- [ ] **Step 5: rustfmt + commit**

```bash
rustfmt crates/athenaeum-core/src/relinking/mod.rs crates/athenaeum-core/src/scanner/mod.rs
git add -A && git commit -m "fix(xplat): relink uses separator-strict byte-range — sibling/case-variant roots no longer swept or rewritten"
```

---

### Task 2: `recreate_calibration_sets_for_root` — byte-range predicate

Fixes C2.

**Files:**
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs:1333-1366` (extract query into helper, switch predicate)
- Test: same file, new test in an existing `#[cfg(test)]` module

**Interfaces:**
- Produces: `fn calibration_frame_rows_under_root(conn: &rusqlite::Connection, root_path: &str) -> Result<Vec<(i64, String)>, rusqlite::Error>` (private to the module; unit-tested directly).

- [ ] **Step 1: Extract + write the failing test**

Add above `recreate_calibration_sets_for_root`:

```rust
/// Calibration-frame `(frame_id, imagetyp)` rows under `root_path`,
/// separator-strict. Extracted from `recreate_calibration_sets_for_root` so
/// the sibling-root boundary is unit-testable without driving the whole
/// set-rebuild machinery. The delete step this rebuild pairs with
/// (`db::delete_calibration_sets_for_root`) has been byte-range-strict since
/// 81aedae7 — the rebuild sweeping wider than the delete folded a sibling
/// root's darks/flats into this root's rebuilt sets.
fn calibration_frame_rows_under_root(
    conn: &rusqlite::Connection,
    root_path: &str,
) -> Result<Vec<(i64, String)>, rusqlite::Error> {
    let (pred, values) =
        crate::db::scan_root_prefix_predicate("f.path", &[root_path.to_string()]);
    let sql = format!(
        "SELECT fr.id, fr.imagetyp FROM frames fr
         JOIN files f ON fr.file_id = f.id
         WHERE ({pred})
           AND fr.imagetyp IN ('Flat','Dark','Bias','DarkFlat','MasterFlat','MasterDark','MasterBias','MasterDarkFlat')"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values.iter()), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect()
}
```

In `recreate_calibration_sets_for_root`, delete the `like_pattern` line and the inline `prepare`/`query_map` (`:1348-1366`), replacing with:

```rust
    let rows = calibration_frame_rows_under_root(&conn, &root_path)?;
    for (frame_id, imagetyp) in rows {
        match imagetyp.as_str() {
            // …existing match arms unchanged…
        }
    }
```

Test (place in a new `mod calibration_rebuild_tests` beside `overview_tests`, using the shared fixture patterns; a minimal `frames` insert is `INSERT INTO frames (file_id, imagetyp) VALUES (?1, ?2)` — all other frame columns are nullable; verify once against `schema.rs::init_db` and extend the insert if a NOT NULL bites):

```rust
#[cfg(test)]
mod calibration_rebuild_tests {
    use super::*;

    #[test]
    fn calibration_frame_query_excludes_sibling_and_case_variant_roots() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let mut mk = |path: &str, imagetyp: &str| {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'd.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![path],
            ).unwrap();
            let fid = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp) VALUES (?1, ?2)",
                rusqlite::params![fid, imagetyp],
            ).unwrap();
            conn.last_insert_rowid()
        };
        let mine = mk(r"C:\Astro\dark1.fits", "Dark");
        let sibling = mk(r"C:\Astro_backup\dark2.fits", "Dark");
        let case_variant = mk(r"C:\astro\dark3.fits", "Dark");

        let rows = calibration_frame_rows_under_root(&conn, r"C:\Astro").unwrap();
        let ids: Vec<i64> = rows.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&mine));
        assert!(!ids.contains(&sibling), "name-prefix sibling root leaked in");
        assert!(!ids.contains(&case_variant), "case-variant root leaked in (LIKE was case-insensitive)");
    }
}
```

- [ ] **Step 2: Run to verify the test fails against the OLD query** — write the test first, keep the old inline query, run:

Run: `cargo test -p athenaeum-core calibration_frame_query`
Expected: compile error (`calibration_frame_rows_under_root` undefined). Then add the helper WITH the old `LIKE` body first if you want a true red — or accept compile-fail as the red step and go straight to the byte-range body (the assertions encode the regression either way).

- [ ] **Step 3: Implement as above, run again**

Run: `cargo test -p athenaeum-core calibration_frame_query`
Expected: PASS.

- [ ] **Step 4: Full-module check + commit**

Run: `cargo test -p athenaeum-core scan_roots`
Expected: PASS (no existing test relies on the wide sweep).

```bash
rustfmt crates/athenaeum-core/src/api/scan_roots.rs
git add -A && git commit -m "fix(xplat): calibration-set rebuild scoped separator-strict to its root — sibling roots' frames stay out"
```

---

### Task 3: Missing-files query — byte-range predicate, both backends

Fixes I1. Requires making the predicate helper `pub` for the web crate.

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations.rs:94` (visibility)
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs:1225-1249` (core query, used by the Tauri path)
- Modify: `crates/athenaeum-web/src/routes/missing_files.rs:110-137` (mirror)
- Test: `crates/athenaeum-core/src/api/scan_roots.rs`

**Interfaces:**
- Produces: `pub fn scan_root_prefix_predicate` (was `pub(crate)`) — web mirror consumes it as `athenaeum_core::db::scan_root_prefix_predicate`.

- [ ] **Step 1: Visibility** — `db/operations.rs:94`: `pub(crate) fn scan_root_prefix_predicate` → `pub fn scan_root_prefix_predicate` (doc comment already explains the contract; append one line: "`pub` for the Axum mirrors, which inline this query shape.").

- [ ] **Step 2: Core site** — in the missing-files function at `api/scan_roots.rs:1225` replace the prepare + `path_prefix` with:

```rust
        let (pred, values) = crate::db::scan_root_prefix_predicate("f.path", &[path.clone()]);
        let sql = format!(
            "SELECT f.id, f.path, f.filename, f.size, f.modified_at,
                        CASE WHEN fr.id IS NOT NULL THEN 1 ELSE 0 END as has_frame,
                        fr.object,
                        fr.date_obs
                 FROM files f
                 LEFT JOIN frames fr ON fr.file_id = f.id
                 WHERE ({pred}) AND f.archived_in_operation IS NULL"
        );
        let mut stmt = conn.prepare(&sql)?;
        let result: Vec<OrphanedFile> = stmt
            .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                // …row mapper unchanged…
            })?
            .collect::<Result<Vec<_>, _>>()?;
```

- [ ] **Step 3: Web mirror** — same substitution in `routes/missing_files.rs:110-137`, with the web error mapping (`.map_err(db_err)`) and `athenaeum_core::db::scan_root_prefix_predicate`. Delete the `path_prefix` line.

- [ ] **Step 4: Test** (core): in a new/existing test module in `api/scan_roots.rs`:

```rust
#[test]
fn find_missing_files_ignores_sibling_root_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
    let root = tmp.path().join("my_astro");
    std::fs::create_dir_all(&root).unwrap();
    let root_str = root.canonicalize().unwrap().to_string_lossy().to_string();
    let added = add_scan_root(&ctx, root_str.clone(), &PathPolicy::AllowAll, None).unwrap();
    {
        let db = ctx.db.get().unwrap();
        let conn = db.conn();
        // Missing file genuinely under the root (never created on disk):
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'gone.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
            rusqlite::params![format!("{root_str}/gone.fits")],
        ).unwrap();
        // Sibling-root row, also absent on disk — must NOT be reported under this root.
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'x.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
            rusqlite::params![format!("{root_str}XY/x.fits")],
        ).unwrap();
    }
    let missing = find_missing_files(&ctx, added.id.unwrap(), &crate::events::NullEmitter).unwrap();
    let paths: Vec<&str> = missing.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.iter().any(|p| p.ends_with("gone.fits")));
    assert!(!paths.iter().any(|p| p.contains("XY")), "sibling-root file reported as missing: {paths:?}");
}
```

Adjust the call to the function's actual name/signature at `api/scan_roots.rs` (the function containing line 1225 — it takes ctx, root id, and an emitter).

Run: `cargo test -p athenaeum-core find_missing_files_ignores`
Expected: PASS. Also `cargo build -p athenaeum-web`: PASS.

- [ ] **Step 5: Commit**

```bash
rustfmt crates/athenaeum-core/src/db/operations.rs crates/athenaeum-core/src/api/scan_roots.rs crates/athenaeum-web/src/routes/missing_files.rs
git add -A && git commit -m "fix(xplat): missing-files scan scoped separator-strict, both backends"
```

---

### Task 4: db/operations.rs prefix hygiene — overview trim, directory listing separator, duplicates attribution

Fixes I2 (`get_folder_overview` 0-files for `C:\`/`/` roots), I3 (`get_files_by_directory{,_for_camera}` build-OS separator + no trim), I5 (`enrich_duplicate_groups` bare `starts_with`).

**Files:**
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs:1590-1598`
- Modify: `crates/athenaeum-core/src/db/operations.rs:856-859`, `:969-972`, `:1723-1727`
- Modify: `crates/athenaeum-core/src/scanner/mod.rs:50` (`path_has_root_prefix` visibility)
- Test: `crates/athenaeum-core/src/api/scan_roots.rs` (`overview_tests`), `crates/athenaeum-core/src/db/operations.rs`

- [ ] **Step 1: Failing test — drive-letter / unix root overview** (append to `overview_tests`, DB-fixture level since `C:\` can't be a real dir on the dev host — insert the scan_roots row directly):

```rust
#[test]
fn overview_counts_files_under_trailing_separator_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
    let db = ctx.db.get().unwrap();
    {
        let conn = db.conn();
        // Roots stored WITH a trailing separator: a Windows drive root and
        // the POSIX filesystem root — both real user configurations.
        conn.execute(
            "INSERT INTO scan_roots (path, enabled) VALUES (?1, 1), (?2, 1)",
            rusqlite::params![r"C:\", "/"],
        ).unwrap();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'a.fits', 100, '2026-01-01T00:00:00Z', 'FITS')",
            rusqlite::params![r"C:\Astro\a.fits"],
        ).unwrap();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'b.fits', 50, '2026-01-01T00:00:00Z', 'FITS')",
            rusqlite::params!["/b.fits"],
        ).unwrap();
    }
    let ov = get_folder_overview(&ctx).unwrap();
    let counts: Vec<i64> = ov.scan_roots.iter().map(|s| s.file_count).collect();
    assert_eq!(counts, vec![1, 1], "trailing-separator roots must still own their descendants: {ov:?}");
}
```

(If `scan_roots` has more NOT NULL columns, extend the insert per `schema.rs` — check once.)

Run: `cargo test -p athenaeum-core overview_counts_files_under_trailing`
Expected: FAIL with `[0, 0]`.

- [ ] **Step 2: Fix overview** (`api/scan_roots.rs:1590-1598`): bind a pre-trimmed root — the only prefix site that skipped the trim every sibling site performs:

```rust
        // Trim any trailing separator BEFORE the SQL appends its own — a root
        // stored as `C:\`, `D:\` or `/` otherwise builds a doubled-separator
        // lower bound that matches nothing (same normalization every other
        // prefix site does via `trim_end_matches`).
        let root_trimmed = root.path.trim_end_matches(['/', '\\']);
        let (file_count, total_bytes): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM files
                 WHERE (path >= ?1 || '/' AND path < ?1 || '0')
                    OR (path >= ?1 || '\\' AND path < ?1 || ']')",
                rusqlite::params![root_trimmed],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
```

Run the test again: PASS. Also run the two existing overview pins: `cargo test -p athenaeum-core overview_` → PASS.

- [ ] **Step 3: Failing test — directory listing with Windows-shaped rows** (in `db/operations.rs` tests):

```rust
#[test]
fn get_files_by_directory_uses_data_separator_and_handles_drive_root() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    for (path, name) in [
        (r"C:\Astro\a.fits", "a.fits"),
        (r"C:\Astro\sub\b.fits", "b.fits"),
        (r"C:\root_level.fits", "root_level.fits"),
    ] {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, ?2, 1, '2026-01-01T00:00:00Z', 'FITS')",
            rusqlite::params![path, name],
        ).unwrap();
    }
    // Direct children only, keyed off the DATA's separator (not the build OS's):
    let in_astro = get_files_by_directory(&conn, r"C:\Astro", None).unwrap();
    assert_eq!(in_astro.len(), 1, "only the direct child, not sub\\b.fits");
    assert_eq!(in_astro[0].0.filename, "a.fits");
    // Drive root with its trailing separator:
    let at_root = get_files_by_directory(&conn, r"C:\", None).unwrap();
    assert_eq!(at_root.len(), 1);
    assert_eq!(at_root[0].0.filename, "root_level.fits");
}
```

Run: `cargo test -p athenaeum-core get_files_by_directory_uses_data`
Expected: FAIL (0 rows on both asserts when built on macOS/Linux).

- [ ] **Step 4: Fix both listing functions** — replace the three prefix lines in `get_files_by_directory` (`:856-859`) and `get_files_by_directory_for_camera` (`:969-972`) identically:

```rust
    // Data-derived separator (native_separator_of doc: the leading character
    // decides, not the build OS) + trailing-separator trim so a drive root
    // (`D:\`) or `/` still lists its direct children.
    let sep = native_separator_of(directory_path).to_string();
    let dir = directory_path.trim_end_matches(['/', '\\']);
    let path_prefix = format!("{}{}", dir, sep);
    let path_hi = path_prefix_upper(&path_prefix);
    let expected_depth = dir.matches(sep.as_str()).count() as i64 + 1;
```

Run the test again: PASS.

- [ ] **Step 5: Duplicates attribution** — `scanner/mod.rs:50`: `fn path_has_root_prefix` → `pub(crate) fn path_has_root_prefix`. In `db/operations.rs:1723-1727` replace the raw prefix `find`:

```rust
            let (scan_root_id, scan_root_path) = scan_roots
                .iter()
                // Separator-boundary-safe (scanner helper): with roots /data
                // and /data/M31x, the file /data/M31xyz/a.fits belongs to
                // /data, not /data/M31x — a bare starts_with mis-attributed it.
                .find(|(_, sr_path)| crate::scanner::path_has_root_prefix(&r.path, sr_path))
                .map(|(id, sr_path)| (Some(*id), Some(sr_path.clone())))
                .unwrap_or((None, None));
```

Add a small test in `db/operations.rs`: two scan_roots (`/data`, `/data/M31x`, remember `ORDER BY length(path) DESC`), one file `/data/M31xyz/a.fits` in a duplicate group of size 2 — assert `scan_root_path == Some("/data")`. If wiring `find_duplicate_groups` end-to-end is heavy (needs hashes), test `path_has_root_prefix` boundary directly instead — it is already pinned in scanner tests, so an integration assert is optional; state which you did in the commit body.

- [ ] **Step 6: Gates + commit**

Run: `cargo test -p athenaeum-core operations && cargo test -p athenaeum-core overview_`
Expected: PASS.

```bash
rustfmt crates/athenaeum-core/src/db/operations.rs crates/athenaeum-core/src/api/scan_roots.rs crates/athenaeum-core/src/scanner/mod.rs
git add -A && git commit -m "fix(xplat): prefix hygiene — overview trailing-separator roots, data-derived listing separator, boundary-safe duplicate attribution"
```

---

### Task 5: Verbatim-path hygiene — `normalize_path` at every escape point

Fixes C4 (`relink_scan_root` persists `\\?\`), I9 (`browse_directories` leaks `\\?\` to UI/export), I10 (web PathPolicy denies everything on Windows).

**Files:**
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs:64` (visibility), `:1091-1098`
- Modify: `crates/athenaeum-core/src/api/mod.rs:95-108` (`PathPolicy::check`)
- Modify: `crates/athenaeum-core/src/api/files.rs:789-835` (`browse_directories`)
- Test: `crates/athenaeum-core/src/api/mod.rs` or `api/scan_roots.rs`

**Interfaces:**
- Produces: `pub fn normalize_path(path: &Path) -> PathBuf` in `athenaeum_core::api::scan_roots` (was private; Tasks 6 and 10 consume it cross-crate).

- [ ] **Step 1: Make `normalize_path` pub** (`api/scan_roots.rs:64`): `fn normalize_path` → `pub fn normalize_path`. Extend its doc: "pub: also used by PathPolicy, browse_directories, and the archive-root commands — every place a canonicalized path escapes or is compared."

- [ ] **Step 2: `relink_scan_root`** (`:1091-1098`) — wrap:

```rust
    // normalize_path: canonicalize() on Windows returns \\?\-verbatim paths;
    // every other path-writing site in this file strips the prefix before the
    // path reaches scan_roots.path / files.path. Relink was the one writer
    // that didn't — splitting one catalog into two spellings of the same tree.
    let canonical = normalize_path(&new_path_buf.canonicalize().map_err(|e| {
        tracing::error!(root_id, path = %new_path, error = %e, "failed to resolve relink target path");
        ApiError::Internal(format!("Failed to resolve path: {}", e))
    })?);
```

- [ ] **Step 3: `PathPolicy::check`** (`api/mod.rs:95-108`) — normalize both sides so no call site can ever pair a verbatim spelling with a normalized one:

```rust
    pub fn check(&self, p: &Path) -> Result<(), ApiError> {
        match self {
            PathPolicy::AllowAll => Ok(()),
            PathPolicy::AllowedRoots(roots) => {
                // Windows canonicalize() yields \\?\-verbatim paths. Candidate
                // and roots are canonicalized at DIFFERENT sites, so one side
                // can be verbatim while the other is not — and
                // Prefix::VerbatimDisk never component-matches Prefix::Disk,
                // turning the sandbox into deny-all. Fold both sides first.
                let candidate = crate::api::scan_roots::normalize_path(p);
                if roots
                    .iter()
                    .any(|r| candidate.starts_with(crate::api::scan_roots::normalize_path(r)))
                {
                    Ok(())
                } else {
                    Err(ApiError::Forbidden(format!(
                        "path {} is outside the allowed roots",
                        p.display()
                    )))
                }
            }
        }
    }
}
```

(No change needed in the two web `allowed_roots_policy` helpers — check() now folds their stored roots per call.)

- [ ] **Step 4: `browse_directories`** (`api/files.rs`) — normalize the canonical target and both containment checks:

```rust
    let canonical = crate::api::scan_roots::normalize_path(
        &target.canonicalize().map_err(|e| ApiError::Invalid(format!("Invalid path: {}", e)))?,
    );

    // Security: validate path is within scope roots (both sides normalized —
    // this is the one canonicalize in the domain whose result ESCAPES to the
    // frontend and comes back through add_scan_root / export_to_wbpp).
    let is_allowed = root_paths.iter().any(|allowed| {
        allowed
            .canonicalize()
            .map(|a| canonical.starts_with(crate::api::scan_roots::normalize_path(&a)))
            .unwrap_or(false)
    });
```

and in the `parent` closure, the same `normalize_path(&a)` inside the `map`. (Child entries derive from the normalized `canonical` via `entry.path()`, so they come out clean automatically.)

- [ ] **Step 5: Tests**

Unix hosts: `normalize_path` is a no-op, so pin the invariants that must hold everywhere, plus a Windows-only verbatim test:

```rust
#[test]
fn path_policy_allows_inside_and_refuses_sibling() {
    let policy = PathPolicy::AllowedRoots(vec![PathBuf::from("/data/M31")]);
    assert!(policy.check(Path::new("/data/M31/x.fits")).is_ok());
    assert!(policy.check(Path::new("/data/M31_Ha/x.fits")).is_err(), "component-wise, not string prefix");
}

#[cfg(windows)]
#[test]
fn path_policy_matches_across_verbatim_and_plain_spellings() {
    let policy = PathPolicy::AllowedRoots(vec![PathBuf::from(r"\\?\C:\data")]);
    assert!(policy.check(Path::new(r"C:\data\x.fits")).is_ok());
    let policy2 = PathPolicy::AllowedRoots(vec![PathBuf::from(r"C:\data")]);
    assert!(policy2.check(Path::new(r"\\?\C:\data\x.fits")).is_ok());
}
```

Run: `cargo test -p athenaeum-core path_policy`
Expected: PASS (the cfg(windows) one compiles out locally; it runs on the Windows CI build).

- [ ] **Step 6: Gates + commit**

Run: `cargo build --workspace` (web + tauri still compile against the now-`pub` fn)
Expected: PASS.

```bash
rustfmt crates/athenaeum-core/src/api/scan_roots.rs crates/athenaeum-core/src/api/mod.rs crates/athenaeum-core/src/api/files.rs
git add -A && git commit -m "fix(xplat): strip Windows verbatim prefix at every escape point — relink root, PathPolicy, browse_directories"
```

---

### Task 6: Archive roots — canonicalize on insert, tolerant lookup

Fixes I16.

**Files:**
- Modify: `crates/athenaeum-tauri/src/commands/archive.rs:62-84` (`add_archive_root`)
- Modify: `crates/athenaeum-web/src/routes/archive.rs:689` region (mirror — same edit)
- Modify: `crates/athenaeum-core/src/archive/root.rs:42-49` (`resolve_archive_root` lookup)
- Test: `crates/athenaeum-core/src/archive/root.rs` (existing tests module)

- [ ] **Step 1: Canonicalize on insert** — in BOTH `add_archive_root` implementations, after the `is_dir` check:

```rust
    // Store the canonical normalized spelling — archive roots were the one
    // root table whose rows were inserted verbatim from the picker, so
    // `C:\Archive` vs `c:\Archive` (or a \\?\-verbatim form) could coexist as
    // two rows and the exact-string lookup in resolve_archive_root rejected
    // legitimate re-picks of the same folder.
    let path = match std::path::Path::new(&path).canonicalize() {
        Ok(c) => athenaeum_core::api::scan_roots::normalize_path(&c)
            .to_string_lossy()
            .to_string(),
        Err(e) => return Err(format!("failed to resolve '{}': {}", path, e)),
    };
```

(web mirror returns its `(StatusCode, String)` shape — adapt the error arm accordingly.)

- [ ] **Step 2: Tolerant lookup** — `archive/root.rs`, replace the `requested` branch:

```rust
    if let Some(p) = requested {
        let known = |candidate: &str| -> Result<bool> {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM archive_roots WHERE path = ?1",
                [candidate],
                |r| r.get::<_, i64>(0),
            )? > 0)
        };
        if known(p)? {
            return Ok(p.to_string());
        }
        // The caller may hand back a different spelling of a configured root
        // (case variant on a case-insensitive FS, verbatim prefix, trailing
        // separator). Retry with the canonical normalized form before rejecting.
        if let Ok(c) = std::path::Path::new(p).canonicalize() {
            let normalized = crate::api::scan_roots::normalize_path(&c)
                .to_string_lossy()
                .to_string();
            if known(&normalized)? {
                return Ok(normalized);
            }
        }
        anyhow::bail!("'{}' is not a configured archive folder", p);
    }
```

- [ ] **Step 3: Test** (append to `root.rs` tests):

```rust
#[test]
fn resolve_accepts_respelled_configured_root() {
    let (conn, settings) = test_ctx();
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().canonicalize().unwrap().to_string_lossy().to_string();
    conn.execute(
        "INSERT INTO archive_roots (path, label, is_default) VALUES (?1, NULL, 1)",
        [&canonical],
    ).unwrap();
    // Same folder, different spelling (trailing separator) — must resolve.
    let respelled = format!("{}{}", canonical, std::path::MAIN_SEPARATOR);
    let resolved = resolve_archive_root(&conn, &settings, Some(&respelled)).unwrap();
    assert_eq!(resolved, canonical);
    // A genuinely unknown folder still errors.
    let other = tempfile::tempdir().unwrap();
    assert!(resolve_archive_root(&conn, &settings, Some(other.path().to_str().unwrap())).is_err());
}
```

Run: `cargo test -p athenaeum-core archive::root`
Expected: PASS (new + existing).

- [ ] **Step 4: Commit**

```bash
rustfmt crates/athenaeum-core/src/archive/root.rs crates/athenaeum-tauri/src/commands/archive.rs crates/athenaeum-web/src/routes/archive.rs
git add -A && git commit -m "fix(xplat): archive roots stored canonical + spelling-tolerant lookup, both backends"
```

---

### Task 7: Restore suggestion — component-wise original-parent derivation

Fixes C3 (restore-to-original dead on Windows).

**Files:**
- Modify: `crates/athenaeum-core/src/archive/path_layout.rs` (new helper + tests)
- Modify: `crates/athenaeum-tauri/src/commands/archive.rs:345-354`
- Modify: `crates/athenaeum-web/src/routes/archive.rs:322-331`

**Interfaces:**
- Produces: `pub fn original_parent_for_restore(source_path: &str, path_in_zip: &str) -> Option<String>` in `athenaeum_core::archive::path_layout`.

- [ ] **Step 1: Failing tests** (in `path_layout.rs` tests):

```rust
#[test]
fn original_parent_component_based_both_separator_styles() {
    // POSIX source vs '/'-separated zip path:
    assert_eq!(
        original_parent_for_restore("/data/Astro/M31/x.fits", "Astro/M31/x.fits"),
        Some("/data".to_string())
    );
    // Windows source vs the SAME '/'-separated zip path — the old string
    // strip_suffix could never match this:
    assert_eq!(
        original_parent_for_restore(r"C:\data\Astro\M31\x.fits", "Astro/M31/x.fits"),
        Some(r"C:\data".to_string())
    );
    // Stripping consumes the whole path -> None (parity with the old code).
    assert_eq!(original_parent_for_restore("/Astro/M31/x.fits", "Astro/M31/x.fits"), None);
    // Fallback two-component zip path over a shallow source: strips both
    // components, leaving the bare drive designator.
    assert_eq!(
        original_parent_for_restore(r"C:\stray\x.fits", "Root/x.fits"),
        Some("C:".to_string())
    );
}
```

- [ ] **Step 2: Implement** (string-based on purpose: `Path::parent` on the dev host would treat a `\`-separated string as ONE component, so the helper must not go through `Path`):

```rust
/// Reverse of [`path_in_zip`] for the restore-suggestion UI: strip as many
/// trailing components off `source_path` as `path_in_zip` carries, yielding
/// the directory the archive layout was rooted at (the scan root's parent).
/// Component-COUNT based and separator-agnostic: `path_in_zip` is always
/// '/'-separated (zip convention) while `source_path` is a native OS path —
/// the old `strip_suffix(&path_in_zip)` string compare could never match a
/// '\'-separated source, so on Windows the "Original location" restore option
/// was permanently disabled and the dialog fell back to relocating the data
/// under an arbitrary scan root.
pub fn original_parent_for_restore(source_path: &str, path_in_zip: &str) -> Option<String> {
    let n = path_in_zip.split('/').filter(|c| !c.is_empty()).count();
    let mut end = source_path.trim_end_matches(['/', '\\']).len();
    for _ in 0..n {
        end = source_path[..end].rfind(['/', '\\'])?;
    }
    let parent = source_path[..end].trim_end_matches(['/', '\\']);
    if parent.is_empty() {
        None
    } else {
        Some(parent.to_string())
    }
}
```

Run: `cargo test -p athenaeum-core original_parent_component`
Expected: PASS.

- [ ] **Step 3: Swap both command sites** — replace the closure body in `get_restore_suggestions` (Tauri `commands/archive.rs:345-354` and web `routes/archive.rs:322-331`):

```rust
            |row| {
                let source_path: String = row.get(0)?;
                let path_in_zip: String = row.get(1)?;
                Ok(athenaeum_core::archive::path_layout::original_parent_for_restore(
                    &source_path,
                    &path_in_zip,
                ))
            },
```

(Verify `path_layout` is exported from `archive/mod.rs` as `pub mod path_layout` — it is consumed cross-crate already via planner? If not currently `pub`, make it so.)

- [ ] **Step 4: Gates + commit**

Run: `cargo build --workspace && cargo test -p athenaeum-core path_layout`
Expected: PASS.

```bash
rustfmt crates/athenaeum-core/src/archive/path_layout.rs crates/athenaeum-tauri/src/commands/archive.rs crates/athenaeum-web/src/routes/archive.rs
git add -A && git commit -m "fix(xplat): restore-to-original works on Windows — component-based original-parent derivation, both backends"
```

---

### Task 8: Restore classification + destination join, planner root match, plan-time collision guard

Fixes I13, I14, I15.

**Files:**
- Modify: `crates/athenaeum-core/src/archive/path_layout.rs` (new `path_starts_with_fold`)
- Modify: `crates/athenaeum-core/src/archive/restore.rs:80-91` (`classify_target`), `:333-337` (dest join)
- Modify: `crates/athenaeum-core/src/archive/planner.rs:130-136`, `:338-344` (root match), plus a uniqueness check after each plan's `files` vec is complete
- Test: `path_layout.rs`, `restore.rs`

**Interfaces:**
- Produces: `pub(crate) fn path_starts_with_fold(path: &Path, root: &Path) -> bool` and `pub(crate) fn dest_under_root(root: &Path, path_in_zip: &str) -> PathBuf` in `path_layout.rs`.

- [ ] **Step 1: Helpers + failing tests** in `path_layout.rs`:

```rust
/// Component-wise "is `path` under (or equal to) `root`", case-folded on
/// case-insensitive hosts (Windows/macOS). Plain `Path::starts_with` is
/// exact-case, which classified `C:\astro\…` as OUTSIDE root `C:\Astro` even
/// though NTFS treats them as one directory — flipping a restore from
/// "put files back" to "relocate under root", and flattening archive layouts.
pub(crate) fn path_starts_with_fold(path: &Path, root: &Path) -> bool {
    if path.starts_with(root) {
        return true;
    }
    if !cfg!(any(windows, target_os = "macos")) {
        return false;
    }
    let comps = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    let (a, b) = (comps(path), comps(root));
    a.len() >= b.len() && a[..b.len()] == b[..]
}

/// Join an always-'/'-separated `path_in_zip` under `root` component-wise, so
/// the resulting (and later CATALOG-PERSISTED) path uses only native
/// separators — `root.join(path_in_zip)` on Windows produced the mixed
/// spelling `C:\root\Lights/M31/x.fits` in `files.path`.
pub(crate) fn dest_under_root(root: &Path, path_in_zip: &str) -> PathBuf {
    let mut d = root.to_path_buf();
    for comp in path_in_zip.split('/').filter(|c| !c.is_empty()) {
        d.push(comp);
    }
    d
}
```

Tests:

```rust
#[test]
fn starts_with_fold_is_component_wise() {
    assert!(path_starts_with_fold(Path::new("/photos/astro/x.fits"), Path::new("/photos/astro")));
    assert!(!path_starts_with_fold(Path::new("/photos/astro2/x.fits"), Path::new("/photos/astro")));
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn starts_with_fold_case_folds_on_case_insensitive_hosts() {
    assert!(path_starts_with_fold(Path::new("/data/astro/x.fits"), Path::new("/data/Astro")));
}

#[test]
fn dest_under_root_joins_component_wise() {
    let d = dest_under_root(Path::new("/r"), "Lights/M31/x.fits");
    let comps: Vec<_> = d.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    assert!(comps.ends_with(&["r".into(), "Lights".into(), "M31".into(), "x.fits".into()]));
}
```

Run: `cargo test -p athenaeum-core starts_with_fold` → compile-fail red, then implement, then PASS.

- [ ] **Step 2: Wire into restore** — `restore.rs`:

`classify_target` (`:80-91`):

```rust
fn classify_target<'a>(
    target_root_path: &'a Path,
    files: &[crate::archive::models::ArchiveOperationFile],
) -> RestoreTargetMode<'a> {
    let all_under = files.iter().all(|f| {
        crate::archive::path_layout::path_starts_with_fold(Path::new(&f.source_path), target_root_path)
    });
    if all_under {
        RestoreTargetMode::Original
    } else {
        RestoreTargetMode::UnderRoot(target_root_path)
    }
}
```

Dest pick (`:334-337`):

```rust
        let dest = match mode {
            RestoreTargetMode::Original => original.to_path_buf(),
            RestoreTargetMode::UnderRoot(root) => {
                crate::archive::path_layout::dest_under_root(root, &f.target_path_in_zip)
            }
        };
```

- [ ] **Step 3: Wire into planner** — both `find`s (`planner.rs:130-136` and `:338-344`):

```rust
        let scan_root = scan_roots
            .iter()
            .find(|r| path_layout::path_starts_with_fold(src, Path::new(r.as_str())))
            .cloned()
            .unwrap_or_else(|| {
                src.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
            });
```

- [ ] **Step 4: Plan-time collision guard** — after each builder's `files` vec is fully populated (immediately before the plan totals / return in BOTH `build_plan` paths), insert:

```rust
    // Two files must never map to one in-zip entry: staging would silently
    // overwrite the first and the archive dies later with a misleading
    // hash-mismatch. Case-insensitive on every platform — zip entries that
    // differ only by case explode on Windows extraction anyway.
    {
        let mut seen = std::collections::HashSet::new();
        for f in &files {
            let key = (f.target_zip_path.to_lowercase(), f.target_path_in_zip.to_lowercase());
            if !seen.insert(key) {
                return Err(anyhow!(
                    "two files map to the same in-zip path '{}' in {} — check for duplicate filenames under unregistered or differently-cased roots",
                    f.target_path_in_zip,
                    f.target_zip_path
                ));
            }
        }
    }
```

- [ ] **Step 5: Gates + commit**

Run: `cargo test -p athenaeum-core archive`
Expected: PASS (existing archive round-trip tests unaffected — unique inputs stay unique).

```bash
rustfmt crates/athenaeum-core/src/archive/path_layout.rs crates/athenaeum-core/src/archive/restore.rs crates/athenaeum-core/src/archive/planner.rs
git add -A && git commit -m "fix(xplat): restore/planner case-folded root matching, native-separator restore paths, plan-time in-zip collision guard"
```

---

### Task 9: Archive — honest deletions

Fixes I17 (swallowed `remove_file`), plus audit Minor M10 (`cleanup_staging` failing a finished archive).

**Files:**
- Modify: `crates/athenaeum-core/src/archive/restore.rs:546`, `:577-580`
- Modify: `crates/athenaeum-tauri/src/commands/archive.rs:584-591` + `crates/athenaeum-web/src/routes/archive.rs` `delete_archive` (line ~610)
- Modify: `crates/athenaeum-core/src/archive/executor.rs:469`

- [ ] **Step 1: restore.rs** — zip cleanup arm (`:546`):

```rust
            if let Err(e) = std::fs::remove_file(zp) {
                tracing::warn!(path = %zp, error = %e, "failed to remove zip after restore; file left in place");
            }
```

`cleanup_temp` (`:577-580`):

```rust
fn cleanup_temp(temp_dir: &Path) {
    if temp_dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(temp_dir) {
            tracing::warn!(path = %temp_dir.display(), error = %e, "failed to remove restore temp dir");
        }
    }
}
```

- [ ] **Step 2: `delete_archive`** — in BOTH backends, replace the silent loop and gate the DB deletes on zip deletion having succeeded (Windows sharing violations otherwise orphan the zip with zero catalog rows pointing at it):

```rust
    let files = adb::list_operation_files(&conn, operation_id).map_err(|e| e.to_string())?;
    let mut seen = std::collections::HashSet::new();
    let mut failed: Vec<String> = Vec::new();
    for f in &files {
        if seen.insert(f.target_zip_path.clone()) {
            let p = std::path::Path::new(&f.target_zip_path);
            if p.exists() {
                if let Err(e) = std::fs::remove_file(p) {
                    tracing::error!(path = %f.target_zip_path, error = %e, "failed to delete archive zip");
                    failed.push(format!("{}: {}", f.target_zip_path, e));
                }
            }
        }
    }
    if !failed.is_empty() {
        return Err(format!(
            "could not delete {} zip file(s); catalog rows left untouched so the archive stays restorable: {}",
            failed.len(),
            failed.join("; ")
        ));
    }
```

(web mirror: same logic, `(StatusCode::INTERNAL_SERVER_ERROR, msg)` error shape. The `p.exists()` guard preserves idempotence for zips already removed manually.)

- [ ] **Step 3: `executor.rs:469`** — staging cleanup after a successful archive must not fail the operation:

```rust
    // Best-effort: at this point every source is verified inside the zip —
    // a locked staging dir (AV holding a handle) must not mark a functionally
    // complete archive Failed and trigger rollback.
    if let Err(e) = staging::cleanup_staging(archive_root, operation_id) {
        tracing::warn!(operation_id, error = %e, "staging cleanup failed after successful archive; leftover .athenaeum_staging dir");
    }
```

- [ ] **Step 4: Gates + commit**

Run: `cargo test -p athenaeum-core archive && cargo build --workspace`
Expected: PASS.

```bash
rustfmt crates/athenaeum-core/src/archive/restore.rs crates/athenaeum-core/src/archive/executor.rs crates/athenaeum-tauri/src/commands/archive.rs crates/athenaeum-web/src/routes/archive.rs
git add -A && git commit -m "fix(xplat): archive deletions honest — sharing-violation-safe delete_archive, warn-not-swallow cleanups"
```

---

### Task 10: file_op semantics — EXDEV fallback, case-only rename, zero-row warn, separator fold

Fixes I6, I7, I8, I4 (+ Minor: `dir_rename_prefixes` trailing trim, `relocate_missing_file` raw path).

**Files:**
- Modify: `crates/athenaeum-core/src/file_op/executor.rs:244-249`, `:418-441`
- Modify: `crates/athenaeum-core/src/file_op/db.rs` (new `set_expected_hash`)
- Modify: `crates/athenaeum-core/src/api/files.rs:692-717`, `:666-669`
- Modify: `crates/athenaeum-core/src/db/operations.rs` (new `normalize_separators`)
- Modify: `crates/athenaeum-tauri/src/commands/missing_files.rs:341-349`
- Test: `api/files.rs`, `db/operations.rs`

**Interfaces:**
- Produces: `pub fn normalize_separators(path: &str) -> String` in `athenaeum_core::db` (pub: the Tauri crate consumes it); `pub fn set_expected_hash(conn, file_row_id: i64, hash: &str) -> Result<()>` in `file_op::db`; `pub(crate) fn is_same_file_case_variant(old: &Path, new: &Path) -> bool` in `api/files.rs`.

- [ ] **Step 1: `normalize_separators`** in `db/operations.rs` (near `native_separator_of`):

```rust
/// Fold '/'-spelled separators in a user/tool-supplied Windows path to the
/// native '\' form before it participates in catalog-path comparisons — the
/// filesystem accepts `C:/Astro/Old` but every stored row spells it
/// `C:\Astro\Old`, so a '/'-spelled input silently updated zero rows.
/// Lossless on Windows (filenames cannot contain '/' or '\'); the identity on
/// POSIX hosts ('\' is a legal filename char there — never rewrite it).
pub fn normalize_separators(path: &str) -> String {
    if cfg!(windows) {
        path.replace('/', "\\")
    } else {
        path.to_string()
    }
}
```

Test (runs meaningfully only on Windows, pin the POSIX identity locally):

```rust
#[test]
fn normalize_separators_is_identity_on_posix() {
    if !cfg!(windows) {
        assert_eq!(normalize_separators(r"/data/weird\name/x.fits"), r"/data/weird\name/x.fits");
    }
}
#[cfg(windows)]
#[test]
fn normalize_separators_folds_forward_slashes() {
    assert_eq!(normalize_separators("C:/Astro/Old"), r"C:\Astro\Old");
}
```

- [ ] **Step 2: `rename_path`** (`api/files.rs:692-717`) — apply the fold and support case-only renames:

At the top of the fn body (after the `new_name` validation):

```rust
    let old_path = crate::db::normalize_separators(&old_path);
```

Add the helper near `dir_rename_prefixes`:

```rust
/// True when `new` names the SAME on-disk file as `old` (case-insensitive
/// filesystems: renaming `m31.fits` → `M31.fits` makes `new.exists()` true
/// for the very file being renamed — that is not a collision).
pub(crate) fn is_same_file_case_variant(old: &Path, new: &Path) -> bool {
    new.exists()
        && old.exists()
        && matches!(
            (std::fs::canonicalize(old), std::fs::canonicalize(new)),
            (Ok(a), Ok(b)) if a == b
        )
}
```

Replace the collision check (`:715-717`):

```rust
    let new = parent.join(&new_name);
    if new.exists() && !is_same_file_case_variant(&old, &new) {
        return Err(ApiError::Conflict(format!("target already exists: {}", new.display())));
    }
```

And harden `dir_rename_prefixes` (`:666-669`) against caller-supplied trailing separators:

```rust
fn dir_rename_prefixes(old_str: &str, new_str: &str) -> (String, String) {
    let sep = crate::db::native_separator_of(old_str);
    let old = old_str.trim_end_matches(sep);
    let new = new_str.trim_end_matches(sep);
    (format!("{old}{sep}"), format!("{new}{sep}"))
}
```

Helper test (in `api/files.rs` tests; keyed on an on-disk probe so it is honest on both case-sensitive and case-insensitive dev volumes):

```rust
#[test]
fn case_variant_identity_check() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.fits");
    std::fs::write(&a, b"x").unwrap();
    let upper = dir.path().join("A.FITS");
    let volume_is_case_insensitive = upper.exists();
    assert_eq!(is_same_file_case_variant(&a, &upper), volume_is_case_insensitive);
    let b = dir.path().join("b.fits");
    std::fs::write(&b, b"y").unwrap();
    assert!(!is_same_file_case_variant(&a, &b), "distinct files are never the same");
}
```

- [ ] **Step 3: EXDEV fallback** — `file_op/db.rs` add:

```rust
/// Backfill `expected_hash` for a row that degraded from AtomicRename to the
/// cross-volume pipeline at execute time (EXDEV) — rename rows are planned
/// without a hash.
pub fn set_expected_hash(conn: &Connection, file_row_id: i64, hash: &str) -> Result<()> {
    conn.execute(
        "UPDATE file_operation_files SET expected_hash = ?1 WHERE id = ?2",
        params![hash, file_row_id],
    )?;
    Ok(())
}
```

`file_op/executor.rs` — replace the rename-failure arm in `commit_move` (`:244-249`):

```rust
    if let Err(e) = fs::rename(source, dest) {
        if e.kind() == std::io::ErrorKind::CrossesDevices {
            // Same device id ≠ rename works: Linux bind mounts share st_dev
            // yet rename(2) returns EXDEV across mount points (reachable in
            // the Docker build's compose volumes), and Windows folder-mounted
            // volumes canonicalize into the hosting drive. Degrade this row
            // to the cross-volume pipeline instead of failing the batch.
            tracing::warn!(src = %source.display(), dest = %dest.display(),
                "rename crossed a device boundary; degrading to copy+verify+delete");
            fdb::update_step(conn, step_id, StepStatus::Done, None,
                Some("EXDEV — degraded to copy+verify+delete"))?;
            return run_cross_volume_fallback(conn, operation_id, f, source, dest);
        }
        let msg = format!("rename {} → {}: {}", source.display(), dest.display(), e);
        fdb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
        fdb::update_file_disposition(conn, f.id, FileDisposition::Failed)?;
        anyhow::bail!(msg);
    }
```

and add below `commit_move`:

```rust
/// EXDEV degradation path: hash the still-present source, persist the hash on
/// the row, then run the standard cross-volume steps. Idempotent on resume:
/// re-entry hits EXDEV again and run_copy_step's prior-Copy-step check treats
/// an existing dest as our own partial.
fn run_cross_volume_fallback(
    conn: &Connection,
    operation_id: i64,
    f: &FileOperationFile,
    source: &Path,
    dest: &Path,
) -> Result<()> {
    let hash = compute_xxhash(source)
        .with_context(|| format!("hashing {} for EXDEV fallback", source.display()))?;
    fdb::set_expected_hash(conn, f.id, &hash)?;
    let mut f2 = f.clone();
    f2.expected_hash = Some(hash);
    run_copy_step(conn, operation_id, &f2, source, dest)?;
    run_verify_step(conn, operation_id, &f2, dest)?;
    run_cross_volume_commit_step(conn, operation_id, &f2, source, dest)
}
```

(`FileOperationFile` already derives `Clone` — `file_op/models.rs:197`. `ErrorKind::CrossesDevices` is stable since 1.85; toolchain is 1.96. std maps both POSIX `EXDEV` and Windows `ERROR_NOT_SAME_DEVICE` onto it.)

- [ ] **Step 4: Zero-row warn** — `sync_catalog_path` (`executor.rs:418-441`), after the id-based fallback:

```rust
    if updated_by_path == 0 && f.catalog_file_id.is_none() {
        // Both mechanisms missing together is the path-spelling-drift
        // signature (sidecar/non-catalog files are expected misses — only
        // catalog-eligible formats get the warn).
        let eligible = new_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| matches!(e.to_ascii_lowercase().as_str(), "fits" | "fit" | "fts" | "xisf"))
            .unwrap_or(false);
        if eligible {
            tracing::warn!(src = %f.source_path, dest = %new_path_str,
                "move hot-sync matched no catalog row for catalog-eligible file");
        }
    }
    Ok(())
```

- [ ] **Step 5: `relocate_missing_file`** (`commands/missing_files.rs:341-349`) — fold separators before persisting:

```rust
    let new_path = athenaeum_core::db::normalize_separators(&new_path);
    // Verify the new path exists
    if !Path::new(&new_path).exists() {
        return Err(format!("File does not exist at path: {}", new_path));
    }
```

- [ ] **Step 6: Gates + commit**

Run: `cargo test -p athenaeum-core file_op && cargo test -p athenaeum-core files && cargo build --workspace`
Expected: PASS.

```bash
rustfmt crates/athenaeum-core/src/file_op/executor.rs crates/athenaeum-core/src/file_op/db.rs crates/athenaeum-core/src/api/files.rs crates/athenaeum-core/src/db/operations.rs crates/athenaeum-tauri/src/commands/missing_files.rs
git add -A && git commit -m "fix(xplat): EXDEV move fallback, case-only rename, hot-sync zero-row warn, separator fold at API boundary"
```

---

### Task 11: Export folder-name sanitizer + path_layout Windows-rule completions

Fixes I18 (`..` escape, reserved names, trailing dots, control chars in export folder names) + audit Minors F6 (COM0/LPT0/superscripts), F8 (unsanitized date segment), and the `token()` order bug.

**Files:**
- Modify: `crates/athenaeum-core/src/archive/path_layout.rs:8-53`, `:129-136`
- Modify: `crates/athenaeum-core/src/export/models.rs:242-268`
- Test: both files' test modules

**Interfaces:**
- Produces: `pub fn windows_safe_component(s: &str, fallback: &str) -> String` in `path_layout.rs`.

- [ ] **Step 1: Extract the Windows-safety tail** in `path_layout.rs` (replacing the inline tail of `sanitize_for_filename`):

```rust
/// Windows-safety tail shared by every generated folder/file-name sanitizer:
/// trim trailing dots/spaces (Win32 silently strips them, desyncing the
/// on-disk name from the catalog's), defuse reserved DOS device basenames
/// (CON/PRN/AUX/NUL/COM0-9/LPT0-9 plus the superscript COM¹²³/LPT¹²³ forms —
/// resolved from the pre-first-dot token), and substitute `fallback` for a
/// component that sanitized away to nothing ("", ".", ".." all end here —
/// ".." would otherwise climb OUT of the chosen output folder).
pub fn windows_safe_component(s: &str, fallback: &str) -> String {
    let out = s.trim_end_matches(['.', ' ']).to_string();
    if out.is_empty() {
        return fallback.to_string();
    }
    let base = out.split('.').next().unwrap_or("");
    let upper = base.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.chars().count() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && matches!(upper.chars().nth(3), Some('0'..='9' | '¹' | '²' | '³')));
    if reserved {
        match out.find('.') {
            Some(i) => format!("{}_{}", &out[..i], &out[i..]),
            None => format!("{out}_"),
        }
    } else {
        out
    }
}
```

Rewire `sanitize_for_filename`'s tail (from the `trim_matches` line down) to:

```rust
    let out = out.trim_matches('_').to_string();
    windows_safe_component(&out, "")
```

Update the existing reserved-name pins (`path_layout.rs:189-209`): `COM0`/`LPT0` and the superscript forms are NOW defused (Microsoft's current reserved list includes them) — flip those asserts; `COM10` stays non-reserved.

Fix `token()` (`:46-53`) — sanitize FIRST so a sanitize-to-empty value falls back to `Unknown` instead of an empty token:

```rust
fn token(value: Option<&str>) -> String {
    let s = sanitize_for_filename(value.unwrap_or(""));
    if s.is_empty() { "Unknown".to_string() } else { s }
}
```

Fix `calibration_zip_dir` (`:129-136`) — sanitize the date segment like the light path does:

```rust
    let date_raw = sanitize_for_filename(date_start.get(..10).unwrap_or(""));
    let date = if date_raw.is_empty() { "unknown-date".to_string() } else { date_raw };
    PathBuf::from("Calibration_Archive").join(cam).join(date)
```

- [ ] **Step 2: Harden `sanitize_display_folder_name`** (`export/models.rs:242-268`) — keep its display semantics (spaces preserved) but drop control chars and route through the shared tail:

```rust
pub fn sanitize_display_folder_name(name: &str) -> String {
    let sanitized: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            ':' | '/' | '\\' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    // Collapse consecutive underscores
    let mut result = String::with_capacity(sanitized.len());
    let mut prev_underscore = false;
    for c in sanitized.chars() {
        if c == '_' {
            if !prev_underscore {
                result.push(c);
            }
            prev_underscore = true;
        } else {
            result.push(c);
            prev_underscore = false;
        }
    }
    // Frame-set names are free user text and become a directory under the
    // user-chosen output folder: ".." must not climb out of it, "CON"/"NUL"
    // must not abort the export on Windows, and a trailing dot must not make
    // Win32 silently create a differently-named directory.
    crate::archive::path_layout::windows_safe_component(&result, "Unknown")
}
```

- [ ] **Step 3: Tests**

`path_layout.rs`:

```rust
#[test]
fn windows_safe_component_covers_current_reserved_list() {
    assert_eq!(windows_safe_component("COM0", "X"), "COM0_");
    assert_eq!(windows_safe_component("LPT0", "X"), "LPT0_");
    assert_eq!(windows_safe_component("LPT²", "X"), "LPT²_");
    assert_eq!(windows_safe_component("COM10", "X"), "COM10");
    assert_eq!(windows_safe_component("M31.", "X"), "M31");
    assert_eq!(windows_safe_component("..", "X"), "X");
    assert_eq!(windows_safe_component("", "X"), "X");
}
```

`export/models.rs`:

```rust
#[test]
fn display_folder_name_is_windows_safe() {
    assert_eq!(sanitize_display_folder_name(".."), "Unknown");
    assert_eq!(sanitize_display_folder_name("CON"), "CON_");
    assert_eq!(sanitize_display_folder_name("M31."), "M31");
    assert_eq!(sanitize_display_folder_name("M31 Panel 1"), "M31 Panel 1");
    assert_eq!(sanitize_display_folder_name("a\u{7}b"), "ab");
}
```

Run: `cargo test -p athenaeum-core path_layout && cargo test -p athenaeum-core export`
Expected: PASS (fix any pre-existing pins that asserted the OLD COM0 behavior; nothing else should move).

- [ ] **Step 4: Commit**

```bash
rustfmt crates/athenaeum-core/src/archive/path_layout.rs crates/athenaeum-core/src/export/models.rs
git add -A && git commit -m "fix(xplat): export folder names Windows-safe — reserved names, trailing dots, control chars, '..' escape; shared windows_safe_component"
```

---

### Task 12: WBPP export — intra-batch destination dedup

Fixes I19 (case-colliding frames silently dropped and counted as organized).

**Files:**
- Modify: `crates/athenaeum-core/src/export/file_organizer.rs:321-339` (loop) + new helpers
- Test: same file

**Interfaces:**
- Produces: `struct DestClaims` (private) with `fn claim(&mut self, rel_dir: &str, filename: &str) -> String`.

- [ ] **Step 1: Failing test**

```rust
#[test]
fn dest_claims_disambiguates_case_collisions() {
    let mut claims = DestClaims::default();
    assert_eq!(claims.claim("lights", "L_0001.fits"), "L_0001.fits");
    assert_eq!(claims.claim("lights", "l_0001.FITS"), "l_0001_2.FITS");
    assert_eq!(claims.claim("lights", "L_0001.fits"), "L_0001_3.fits");
    // Different directory — no rename.
    assert_eq!(claims.claim("FLAT_1", "L_0001.fits"), "L_0001.fits");
}
```

- [ ] **Step 2: Implement**

```rust
/// Destinations claimed within one export run, keyed case-insensitively —
/// on NTFS/APFS `L_0001.fits` and `l_0001.FITS` are ONE file, so the second
/// placement used to hit copy_or_link's exists-skip, get counted as
/// organized, and silently vanish from the export.
#[derive(Default)]
struct DestClaims(std::collections::HashSet<String>);

impl DestClaims {
    fn claim(&mut self, rel_dir: &str, filename: &str) -> String {
        let key = |f: &str| format!("{}/{}", rel_dir.to_lowercase(), f.to_lowercase());
        if self.0.insert(key(filename)) {
            return filename.to_string();
        }
        let mut n = 2;
        loop {
            let candidate = match filename.rsplit_once('.') {
                Some((stem, ext)) => format!("{stem}_{n}.{ext}"),
                None => format!("{filename}_{n}"),
            };
            if self.0.insert(key(&candidate)) {
                return candidate;
            }
            n += 1;
        }
    }
}
```

Wire into the loop (`:321-339`): before the loop `let mut claims = DestClaims::default();`, and inside:

```rust
        let filename = claims.claim(&placement.rel_dir, &placement.filename);
        let dest = dest_dir.join(&filename);
        match copy_or_link(&placement.file_path, &dest, use_symlinks) {
            Ok(_) => {
                files_organized += 1;
                emit_progress(files_organized as usize, Some(&filename));
            }
            Err(e) => warnings.push(format!("Failed to copy {}: {}", filename, e)),
        }
```

(The exists-skip inside `copy_or_link` stays — it is what makes a RE-export over an existing tree idempotent; within one run it can no longer eat a distinct frame.)

- [ ] **Step 3: Run + commit**

Run: `cargo test -p athenaeum-core file_organizer`
Expected: PASS (new + existing placement pins).

```bash
rustfmt crates/athenaeum-core/src/export/file_organizer.rs
git add -A && git commit -m "fix(xplat): WBPP export dedups case-colliding destinations instead of silently dropping frames"
```

---

### Task 13: Windows long-path manifest

Fixes I20. Verified against vendored `tauri-build 2.6.3`: `WindowsAttributes::app_manifest<S: AsRef<str>>` + `Attributes::windows_attributes` + `try_build` all exist; a custom manifest REPLACES Tauri's default, whose Common-Controls dependency must be restated.

**Files:**
- Modify: `crates/athenaeum-tauri/build.rs:41`

- [ ] **Step 1: Replace `tauri_build::build();`**

```rust
    // Windows: opt the exe into long paths (>260 chars). Deep generated trees
    // (calibration-library outputs, archive staging/restore temp) plausibly
    // exceed MAX_PATH; with the manifest + the OS LongPathsEnabled policy the
    // Win32 limit lifts. A custom manifest REPLACES Tauri's default, so its
    // Common-Controls dependency is restated verbatim (WebView2 dialogs need it).
    let windows = tauri_build::WindowsAttributes::new().app_manifest(
        r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"
      />
    </dependentAssembly>
  </dependency>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings xmlns:ws2="http://schemas.microsoft.com/SMI/2016/WindowsSettings">
      <ws2:longPathAware>true</ws2:longPathAware>
    </windowsSettings>
  </application>
</assembly>"#,
    );
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
        .expect("failed to run tauri-build");
```

- [ ] **Step 2: Verify + commit**

Run: `cargo build -p athenaeum-tauri` (macOS: attributes are inert but the code must compile)
Expected: PASS.

```bash
rustfmt crates/athenaeum-tauri/build.rs
git add -A && git commit -m "fix(xplat): longPathAware manifest for the Windows bundle"
```

Note for the release docs (Task 18): effectiveness also requires the OS policy `HKLM\SYSTEM\CurrentControlSet\Control\FileSystem\LongPathsEnabled=1` — mention in the download-page/FAQ when 0.5.1 ships.

---

### Task 14: FITS writer — Windows sharing-violation retry on atomic replace

Fixes I21.

**Files:**
- Modify: `crates/athenaeum-core/src/fits_writer/writer.rs:73`

- [ ] **Step 1: Implement**

```rust
/// `fs::rename` replaces an existing destination on every platform (Windows:
/// MOVEFILE_REPLACE_EXISTING), but on Windows it fails with a sharing
/// violation while another process (AV real-time scan, indexer, a stacker
/// with the master open) holds the destination without FILE_SHARE_DELETE —
/// POSIX rename never does. Bounded retry: 5 attempts, 50→800 ms backoff.
#[cfg(windows)]
fn rename_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    let mut delay = std::time::Duration::from_millis(50);
    let mut last: Option<std::io::Error> = None;
    for _ in 0..5 {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(ERROR_SHARING_VIOLATION) | Some(ERROR_ACCESS_DENIED)
                ) =>
            {
                last = Some(e);
                std::thread::sleep(delay);
                delay *= 2;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.expect("loop ran at least once"))
}

#[cfg(not(windows))]
fn rename_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}
```

and at `:73`: `if let Err(e) = rename_replace(&tmp, path) {`.

- [ ] **Step 2: Verify + commit**

Run: `cargo test -p athenaeum-core fits_writer`
Expected: PASS (incl. the concurrent-writers test).

```bash
rustfmt crates/athenaeum-core/src/fits_writer/writer.rs
git add -A && git commit -m "fix(xplat): bounded retry on Windows sharing violations in atomic FITS replace"
```

---

### Task 15: Reversible identity encoding in calibrated-light header cards

Fixes I22 (non-ASCII → `?` breaks adoption forever; Cyrillic Windows profile paths are the owner's own field case) and subsumes the CONTINUE-boundary space loss for these cards (encoded values contain no spaces; the single uuid/path separator space sits at fixed index 36, never on a 67-char chunk boundary).

**Files:**
- Modify: `crates/athenaeum-core/src/fits_parser/calibrated_light.rs` (encode/decode pair + decode application)
- Modify: `crates/athenaeum-core/src/calibration_library/light_headers.rs:77-83`, `:104-109`
- Test: both files

**Interfaces:**
- Produces: `pub fn encode_ident(s: &str) -> String`, `pub fn decode_ident(s: &str) -> String` in `athenaeum_core::fits_parser::calibrated_light`.

- [ ] **Step 1: Failing round-trip tests** (in `calibrated_light.rs`):

```rust
#[test]
fn ident_encoding_round_trips_non_ascii_and_spaces() {
    let cases = [
        r"C:\Users\Вилен\Файл 1.fits",
        "L_0001.fits",
        "name with spaces.fits",
        "50%_done.fits",
    ];
    for c in cases {
        let enc = encode_ident(c);
        assert!(enc.bytes().all(|b| (0x21..=0x7E).contains(&b)), "no spaces/non-ASCII in {enc}");
        assert_eq!(decode_ident(&enc), c, "round trip of {c}");
    }
    // Legacy plain values (written before encoding existed) pass through:
    assert_eq!(decode_ident("L_0001.fits"), "L_0001.fits");
    assert_eq!(decode_ident("50%_x.fits"), "50%_x.fits", "bare % + non-hex stays verbatim");
}
```

- [ ] **Step 2: Implement** in `calibrated_light.rs`:

```rust
/// Reversible ASCII encoding for identity-bearing header values (`ATH_CSRN`
/// and the path half of `ATH_C{DRK,FLT,BIA}`). FITS string values must be
/// printable ASCII; the writer's lossy '?' fallback destroyed non-ASCII
/// identities (a Cyrillic Windows profile path), so scanner adoption could
/// never match them again. Encodes every byte outside 0x21..=0x7E plus '%'
/// itself as %XX — the output has no spaces, so CONTINUE chunk boundaries
/// can't eat a significant space either. Plain ASCII values without '%'
/// round-trip unchanged, which keeps already-written headers readable.
pub fn encode_ident(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if (0x21..=0x7E).contains(&b) && b != b'%' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Inverse of [`encode_ident`]. Only well-formed `%XX` hex pairs decode;
/// everything else passes through verbatim, so legacy values containing a
/// bare '%' survive. (A legacy value containing a LITERAL `%XX` hex triplet
/// mis-decodes — accepted: filenames like that are vanishingly rare and the
/// uuid key is tried before the filename either way.)
pub fn decode_ident(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).to_string())
}
```

Apply on parse: `source_filename: non_empty(keys, "ATH_CSRN").map(|s| decode_ident(&s))`, and in `parse_master_ref`: `Some(MasterRef { uuid, path: decode_ident(&path) })`.

- [ ] **Step 3: Producers** — `light_headers.rs`:

```rust
use crate::fits_parser::calibrated_light::encode_ident;

fn master_ref_card(
    keyword: &str,
    uuid: &str,
    path: &str,
) -> std::result::Result<Card, FitsWriteError> {
    // Path percent-encoded (see encode_ident): keeps non-ASCII identities
    // reversible and the value space-free past the fixed uuid separator.
    Card::new(keyword, CardValue::Str(format!("{uuid} {}", encode_ident(path))))
}
```

and the `ATH_CSRN` card: `CardValue::Str(encode_ident(&inputs.source_filename))`.

Update the existing `cyrillic_master_path_sanitized_not_fatal` test to assert the ROUND TRIP now (build card → format_card succeeds → parse back via `calibrated_light_identity` → path equals the original Cyrillic string) instead of asserting `?` replacement.

- [ ] **Step 4: Cross-checks**

Run: `grep -rn "ATH_CSRN\|ATH_CDRK" crates/ --include=*.rs | grep -v test` — confirm the ONLY producer is `light_headers.rs` and the only consumers go through `calibrated_light.rs` parsing (`scanner::reconcile_calibrated_light`, `resolve_master_set_id` consume the parsed struct — they now receive decoded values automatically). If any other reader parses these cards raw, decode there too.

Run: `cargo test -p athenaeum-core calibrated_light && cargo test -p athenaeum-core light_headers && cargo test -p athenaeum-core lights`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
rustfmt crates/athenaeum-core/src/fits_parser/calibrated_light.rs crates/athenaeum-core/src/calibration_library/light_headers.rs
git add -A && git commit -m "fix(xplat): reversible percent-encoding for identity header cards — non-ASCII paths adopt correctly"
```

---

### Task 16: Frontend — duplicates keep-rules separator fold + depth-first shortest-path

Fixes I23 (path_contains never matches on Windows → silently different deletion set) and audit Minor M1 (UNC copy systematically deleted).

**Files:**
- Modify: `src/components/duplicates/keepRules.ts:85-135`

- [ ] **Step 1: Implement**

Top of file (near the other helpers):

```ts
/** Fold both separator styles to '/'. User patterns like "Backup/2023" must
 *  match Windows catalog paths (`C:\...\Backup\2023\...`); an unmatched rule
 *  silently ABSTAINS and the chain falls through to a different rule — the
 *  user gets a different deletion set than configured. */
const normalizeSeparators = (s: string) => s.replace(/\\/g, '/');

/** Path depth in segments, separator-agnostic. */
const pathDepth = (p: string) => p.split(/[/\\]/).filter(Boolean).length;
```

`evalPathContains` — normalize both sides:

```ts
  const normalizedPatterns = cleaned
    .map(normalizeSeparators)
    .map((p) => (caseSensitive ? p : p.toLowerCase()));

  const out = new Set<number>();
  for (const f of files) {
    const folded = normalizeSeparators(f.path);
    const haystack = caseSensitive ? folded : folded.toLowerCase();
    if (normalizedPatterns.some((p) => haystack.includes(p))) {
      out.add(f.fileId);
    }
  }
  return out;
```

`evalShortestPath` — segment count first, character length as tie-break (character length alone made a UNC copy "longer" than a drive-letter copy of equal depth, so the network copy was systematically marked for deletion):

```ts
function evalShortestPath(files: DuplicateFile[]): Set<number> {
  let bestDepth = Number.POSITIVE_INFINITY;
  let bestLen = Number.POSITIVE_INFINITY;
  for (const f of files) {
    const d = pathDepth(f.path);
    if (d < bestDepth || (d === bestDepth && f.path.length < bestLen)) {
      bestDepth = d;
      bestLen = f.path.length;
    }
  }
  const out = new Set<number>();
  for (const f of files) {
    const d = pathDepth(f.path);
    if (d > bestDepth || (d === bestDepth && f.path.length > bestLen)) out.add(f.fileId);
  }
  return out;
}
```

- [ ] **Step 2: Verify** (no JS test runner in this repo — typecheck + a scratch probe):

Run: `npx tsc --noEmit`
Expected: clean.

Write `/private/tmp/claude-501/…/scratchpad/keeprules-probe.mjs` pasting the two helpers + a Windows-path fixture asserting: pattern `Backup/2023` matches `C:\x\Backup\2023\a.fits`; UNC vs local of equal depth ties on depth and falls to char length. Run `node keeprules-probe.mjs`; expected: all asserts pass.

- [ ] **Step 3: Commit**

```bash
git add src/components/duplicates/keepRules.ts && git commit -m "fix(xplat): duplicates keep-rules — separator-folded path matching, depth-first shortest-path"
```

---

### Task 17: Frontend — UNC breadcrumb + curated path minors

Fixes I24 (UNC breadcrumbs rebuild a relative path) + audit Minors: BlackHole/MissingMetadata root-edge parents, GTK dialog filter case, missing path tooltips.

**Files:**
- Modify: `src/components/dualpane/DualPaneFileBrowser.tsx:2003-2016`
- Modify: `src/pages/BlackHole.tsx:124-128`
- Modify: `src/components/missing-metadata/MissingMetadataTable.tsx:5-10`
- Modify: `src/components/MissingFilesPanel.tsx:137-140`
- Modify (tooltips, `title=` additions only): `src/pages/Settings.tsx:1302,1316`, `src/components/FolderBrowserModal.tsx:167-169`, `src/components/archive/ArchiveDispositionDialog.tsx:124,135-137`, `src/components/folders/MonitoredInspector.tsx:68`, `src/components/folders/ArchiveInspector.tsx:60`, `src/components/dualpane/CatalogSearch.tsx:112`

- [ ] **Step 1: Breadcrumb** (`DualPaneFileBrowser.tsx:2003-2016`):

```tsx
function Breadcrumb({ path, root, onNavigate }: BreadcrumbProps) {
  if (!path || !root) return null;
  const sep = path.includes('\\') ? '\\' : '/';
  // Preserve the absolute prefix: '/' for POSIX, '\\' for UNC shares —
  // filter(Boolean) eats the leading empty segments of '\\nas\astro' and the
  // old re-add branch only restored a POSIX '/', rebuilding a RELATIVE path
  // whose breadcrumb clicks then failed.
  const lead = path.match(/^[/\\]+/)?.[0] ?? '';
  const allParts = path.split(/[/\\]/).filter(Boolean);
  const rootParts = root.split(/[/\\]/).filter(Boolean);
  const descendantParts = allParts.slice(rootParts.length);
  const atRoot = descendantParts.length === 0;
  const segments = descendantParts.map((part, i) => {
    const fullParts = [...rootParts, ...descendantParts.slice(0, i + 1)];
    const target = lead + fullParts.join(sep);
    return { name: part, path: target };
  });
```

(rest of the component unchanged.)

- [ ] **Step 2: Root-edge parent helpers**

`BlackHole.tsx:125-128`:

```tsx
  // Get folder path from full file path. Root edges: 'C:\a.fits' → 'C:\',
  // '/a.fits' → '/' (the old `lastSlash > 0` check returned the whole path
  // for POSIX-root files and drive-relative 'C:' for drive-root files).
  function getFolderPath(fullPath: string): string {
    const lastSlash = Math.max(fullPath.lastIndexOf('/'), fullPath.lastIndexOf('\\'));
    if (lastSlash < 0) return fullPath;
    if (lastSlash === 0) return fullPath[0];
    const head = fullPath.substring(0, lastSlash);
    return /^[A-Za-z]:$/.test(head) ? head + '\\' : head;
  }
```

`MissingMetadataTable.tsx:6-10` — same shape:

```tsx
/** Returns the parent directory of a file path (without trailing slash;
 *  root edges: '/a.fits' → '/', 'C:\a.fits' → 'C:\'). */
function dirname(filePath: string): string {
  const idx = Math.max(filePath.lastIndexOf('/'), filePath.lastIndexOf('\\'));
  if (idx < 0) return '.';
  if (idx === 0) return filePath[0];
  const head = filePath.slice(0, idx);
  return /^[A-Za-z]:$/.test(head) ? head + '\\' : head;
}
```

- [ ] **Step 3: GTK dialog filters** (`MissingFilesPanel.tsx:138`): GTK file-dialog filters are case-sensitive globs — a `.FITS` file is invisible on Linux:

```ts
          { name: 'FITS/XISF Files', extensions: ['fits', 'fit', 'xisf', 'FITS', 'FIT', 'XISF'] },
```

- [ ] **Step 4: Tooltips** — at each listed truncation site, add `title={<the full path variable in scope>}` to the truncating element (pattern precedent: `ExportTab.tsx:426`). Pure attribute additions; no layout changes.

- [ ] **Step 5: Verify + commit**

Run: `npx tsc --noEmit`
Expected: clean. Spot-check the Files browser + Black Hole pages render (`npm run dev` smoke is enough; no behavioral change on POSIX paths).

```bash
git add -A && git commit -m "fix(xplat): UNC breadcrumbs, root-edge parent derivation, dialog filter case, path tooltips"
```

---

### Task 18: Documentation truth-up

**Files:**
- Modify: `CLAUDE.md` (Dual-Pane File Browser section)
- Modify: `docs/superpowers/research/2026-07-30-cross-platform-path-audit.md`

- [ ] **Step 1: CLAUDE.md** — the file_op description drifted from the code (audit "Stale documentation" section):
  - `MoveStrategy { AtomicRename, CopyVerifyDelete, Delete }` → `MoveStrategy { AtomicRename, CopyVerifyDelete }` (+ note: EXDEV at execute time degrades AtomicRename to CopyVerifyDelete).
  - Remove `enqueue_delete_operation` / `cancel_file_operation` from the command list; note user-facing delete is the Black Hole flow.
  - Remove "Delete planner records every subdirectory for deepest-first rmdir".
  - `OperationKind { ZipArchive, FileOpMove, FileOpDelete }` → `OperationKind { ZipArchive, FileOpMove, FileOpReconcile }`.
  - Rephrase "Survives macOS `/Volumes` vs `/private/Volumes` edge cases" → "Survives path-spelling variance structurally: planner stores and executor matches the scanner's own non-canonicalized spelling (no canonicalize on the hot-sync path)".

- [ ] **Step 2: Audit doc** — append a `## Status (2026-07-30 fix cycle)` section mapping each C/I finding to "fixed in <commit short-hash>" or "deferred" (deferred list: frontend platform-detection for symlink checkbox, `use_symlinks` server-side reject, path-util consolidation `src/utils/path.ts`, GROUP_CONCAT `'|'` transport, non-UTF-8 discovery filter, component-length caps, `path_in_zip` backslash cfg-gate, in-zip Windows sanitization for foreign-extraction, staging path-length trims, `browse_directories` `/` sentinel display, TS path sort unification, case-folded root equality in Folders UI).

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md docs/superpowers/research/2026-07-30-cross-platform-path-audit.md
git commit -m "docs: truth-up file_op section in CLAUDE.md; audit status ledger"
```

---

### Task 19: Final gates

- [ ] **Step 1: Full workspace gates**

```bash
cargo build --workspace
cargo test --workspace
npx tsc --noEmit
```

Expected: all green. Fix regressions before proceeding; re-run.

- [ ] **Step 2: Windows compile evidence** — the local cross-check dies in `ring`'s MSVC build (audit note), so the compiler-level Windows proof comes from CI. Flag for the owner in the final report: the next tag build (or a manual `cargo check --workspace` on the Windows box) converts the by-inspection claim into a compiler fact. The only new `#[cfg(windows)]` code is Task 13 (manifest string) and Task 14 (`rename_replace` — std-only APIs) plus the Task 5 test; review those three once against that lens.

- [ ] **Step 3: Review** — run `superpowers:requesting-code-review` over the whole branch diff (plan-verbatim seams: the cross-task contracts are `scan_root_prefix_predicate` pub, `normalize_path` pub, `windows_safe_component`, `path_starts_with_fold`/`dest_under_root`, `encode_ident`/`decode_ident`, `normalize_separators`, `set_expected_hash`).

---

## Deferred (documented, not in this plan)

Everything in the audit's Minor tier not explicitly folded in above — see Task 18 Step 2 list. Two Important-adjacent items deliberately deferred with reasons:
- **`use_symlinks` server-side reject on Windows web hosts** (audit F12): web-on-Windows is not a shipped configuration; revisit if it becomes one.
- **Frontend OS detection via backend instead of userAgent** (audit M6-frontend): needs a tiny new command in both backends; batch it with the next command-surface change.
