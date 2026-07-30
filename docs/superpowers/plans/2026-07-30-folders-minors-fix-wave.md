# Folders Minors Fix Wave (pre-push, 0.5.1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the fix-before-push subset of the Folders-redesign deferred-minors audit (2026-07-30): the confirmed sibling-purge data-loss bug (3 byte-range sites), durable `calibration.library_dir` handling (relink-follows-root + switch-failure demote), three frontend safety fixes, and a zero-risk cosmetic sweep — before pushing branch `0.5.1`.

**Architecture:** All backend changes live in `athenaeum-core` (no Tauri/Axum surface changes — the two-backend rule adds no work), fixed RED-first where a failing test is constructible. Frontend changes are confined to `src/components/folders/`, `src/pages/FileManager.tsx`, and one dead-file deletion; gate is `tsc` + vite build.

**Tech Stack:** Rust (rusqlite, tracing, anyhow→ApiError), React/TS, Tailwind design tokens.

## Global Constraints

- Branch: `0.5.1` (version-branch rule); commit as `eg013ra1n <vilen.sharifov@gmail.com>` — never Claude as author/co-author. Push only to `origin` (GitLab).
- Gates (per repo rules, clippy is NOT a gate): `cargo build --workspace`, `cargo test -p athenaeum-core`, `npx tsc --noEmit`, `npm run build`.
- Formatting: `rustfmt --edition 2021 <files>` on touched files only (bare `cargo fmt -p` breaks on these files — T1 implementer note in the SDD ledger).
- Logging: `tracing` only, message = short stable phrase, data in snake_case fields from the canonical dictionary (`root_id`, `path`, `src`, `dest`, `error`, `kind`, `reason`...). Never swallow errors.
- Frontend: design tokens only (`bg-surface`, `text-error`, `accent-accent`...), backend access via the `api` object, notifications via `notify()`.
- Audit source of truth: `.superpowers/sdd/folders-minors-rollup.md` (rewritten 2026-07-30 with per-item verdicts). Item numbers below (`#6`, `#14`, ...) refer to that file.

---

### Task 1: Separator-strict byte-ranges in `db/operations.rs` (⭐#6 sibling-purge fix + #10 boundary pins)

The bug, confirmed by direct SQLite execution: `path_prefix_upper(&path)` is applied to the root path **without a trailing separator**, so the range for root `/data/M31` is `["/data/M31", "/data/M32")` — which contains sibling root `/data/M31_Ha/*` (`'_'` 0x5F sorts above `'/'` 0x2F). `add_scan_root`'s overlap guard uses component-wise `Path::starts_with`, so name-prefix sibling roots are a legal, ordinary configuration. Three destructive sites share the pattern; two of them run on every scan.

The separator-safe reference implementation already exists in the same file: `frame_ids_under_paths` (`operations.rs:3019-3021`) and `native_separator_of` (`operations.rs:74-76`).

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations.rs` (sites at ~390, ~468, ~577; tests appended to the existing test module near `delete_scan_root_cascade_does_not_cross_match_wildcard_siblings` at ~4956)
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs` (overview boundary tests only, in `overview_tests`)

**Interfaces:**
- Consumes: `native_separator_of(&str) -> char`, `path_prefix_upper(&str) -> Option<String>` (both already `pub(crate)` in `operations.rs`).
- Produces: no signature changes — behavior fix only.

- [ ] **Step 1: Write the failing tests (RED)**

Append to the test module in `crates/athenaeum-core/src/db/operations.rs`, next to `delete_scan_root_cascade_does_not_cross_match_wildcard_siblings` (~line 4956), reusing that module's `insert_file` / `all_paths` helpers. For the frames/calibration-set fixtures, mirror the insert style of the neighboring `delete_scan_root_preserves_master_source_lineage` test (~line 4980) if the raw SQL below doesn't match the schema's NOT NULL set.

```rust
#[test]
fn delete_scan_root_does_not_purge_name_prefix_sibling_root() {
    // Without a trailing separator the byte range for /data/M31 is
    // ["/data/M31", "/data/M32"), which CONTAINS the sibling root
    // /data/M31_Ha ('_' 0x5F sorts above '/' 0x2F). The predicate must
    // require the separator so only true descendants are swept.
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    conn.execute("INSERT INTO scan_roots (path) VALUES ('/data/M31')", []).unwrap();
    let root_id: i64 = conn
        .query_row("SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
        .unwrap();
    insert_file(&conn, "/data/M31/a.fits");
    insert_file(&conn, "/data/M31_Ha/b.fits");

    delete_scan_root(&conn, root_id).unwrap();

    assert_eq!(
        all_paths(&conn),
        vec!["/data/M31_Ha/b.fits".to_string()],
        "sibling root sharing the name prefix must survive the cascade"
    );
}

#[test]
fn delete_scan_root_does_not_purge_name_prefix_sibling_root_windows() {
    // Windows arm: range for C:\Astro without separator is
    // ["C:\Astro", "C:\Astrp") — contains C:\Astro_backup. With the
    // separator it is ["C:\Astro\", "C:\Astro]") — excludes it.
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    conn.execute("INSERT INTO scan_roots (path) VALUES ('C:\\Astro')", []).unwrap();
    let root_id: i64 = conn
        .query_row("SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
        .unwrap();
    insert_file(&conn, "C:\\Astro\\a.fits");
    insert_file(&conn, "C:\\Astro_backup\\b.fits");

    delete_scan_root(&conn, root_id).unwrap();

    assert_eq!(all_paths(&conn), vec!["C:\\Astro_backup\\b.fits".to_string()]);
}

#[test]
fn reconcile_unique_camera_leaves_name_prefix_sibling_untouched() {
    // reconcile runs on EVERY scan — pre-fix it suffixes the SIBLING
    // root's frames.instrume and then deletes the sibling's calibration
    // sets via the same broken range.
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    conn.execute("INSERT INTO scan_roots (path, unique_camera) VALUES ('/data/M31', 1)", [])
        .unwrap();
    let root_id: i64 = conn
        .query_row("SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
        .unwrap();
    insert_file(&conn, "/data/M31/a.fits");
    insert_file(&conn, "/data/M31_Ha/b.fits");
    conn.execute(
        "INSERT INTO frames (file_id, instrume) SELECT id, 'CAM' FROM files",
        [],
    )
    .unwrap();

    reconcile_unique_camera_instrume(&conn, root_id).unwrap();

    let own: String = conn
        .query_row(
            "SELECT fr.instrume FROM frames fr JOIN files f ON fr.file_id = f.id WHERE f.path = '/data/M31/a.fits'",
            [], |r| r.get(0),
        )
        .unwrap();
    assert_eq!(own, format!("CAM N{root_id}"));
    let sibling: String = conn
        .query_row(
            "SELECT fr.instrume FROM frames fr JOIN files f ON fr.file_id = f.id WHERE f.path = '/data/M31_Ha/b.fits'",
            [], |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sibling, "CAM", "sibling root's frames must not receive the suffix");
}

#[test]
fn delete_calibration_sets_for_root_spares_name_prefix_sibling_sets() {
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    conn.execute("INSERT INTO scan_roots (path) VALUES ('/data/M31')", []).unwrap();
    let root_id: i64 = conn
        .query_row("SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
        .unwrap();
    insert_file(&conn, "/data/M31_Ha/dark.fits");
    conn.execute(
        "INSERT INTO frames (file_id) SELECT id FROM files WHERE path = '/data/M31_Ha/dark.fits'",
        [],
    )
    .unwrap();
    let frame_id: i64 = conn
        .query_row("SELECT id FROM frames ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
        .unwrap();
    // Mirror delete_scan_root_preserves_master_source_lineage's calibration_set
    // insert if this named-column form misses a NOT NULL column.
    conn.execute("INSERT INTO calibration_set (frame_type) VALUES ('DARK')", []).unwrap();
    let set_id: i64 = conn
        .query_row("SELECT id FROM calibration_set ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
        .unwrap();
    conn.execute(
        "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
        params![set_id, frame_id],
    )
    .unwrap();

    let deleted = delete_calibration_sets_for_root(&conn, root_id).unwrap();

    assert_eq!(deleted, 0, "no set lives under /data/M31");
    let sets: i64 = conn
        .query_row("SELECT COUNT(*) FROM calibration_set", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sets, 1, "sibling's calibration set must survive");
}
```

- [ ] **Step 2: Run the new tests, verify they FAIL**

Run: `cargo test -p athenaeum-core name_prefix_sibling -- --nocapture`
Expected: 4 failures — the sibling rows are purged / suffixed / counted by the unfixed ranges. (If a fixture INSERT errors on schema instead, fix the fixture per the neighboring tests until the failure is the assertion.)

- [ ] **Step 3: Fix the three destructive sites**

In `crates/athenaeum-core/src/db/operations.rs`, apply the `frame_ids_under_paths` pattern (separator-carrying prefix) at each site:

**Site A — `delete_calibration_sets_for_root` (~line 390):** replace

```rust
    let path_hi = path_prefix_upper(&root_path);
```

with

```rust
    // Byte-range bounds must carry the trailing separator: without it the
    // range for /lib_old also swallows sibling /lib_old2/* (see the
    // name_prefix_sibling tests). Same pattern as frame_ids_under_paths.
    let sep = native_separator_of(&root_path);
    let root_prefix = format!("{}{}", root_path.trim_end_matches(sep), sep);
    let path_hi = path_prefix_upper(&root_prefix);
```

and rebind both predicates (~lines 401 and 448): `params![root_path, path_hi]` → `params![root_prefix, path_hi]`.

**Site B — `reconcile_unique_camera_instrume` (~line 468):** replace

```rust
    let path_hi = path_prefix_upper(&root_path);
```

with the same three lines (using `root_path`), then rebind the three predicates:
- ~line 486: `params![suffix, root_path, path_hi]` → `params![suffix, root_prefix, path_hi]`
- ~line 500: `params![suffix_len, suffix, root_path, path_hi]` → `params![suffix_len, suffix, root_prefix, path_hi]`
- ~line 526: `params![root_path, path_hi]` → `params![root_prefix, path_hi]`

**Site C — `delete_scan_root` (~lines 577-581):** replace the pre-compute block with

```rust
    // Pre-compute file IDs to delete (single byte-range query). The prefix
    // carries the trailing separator so name-prefix siblings (/lib_old vs
    // /lib_old2) are never swept — see the name_prefix_sibling tests.
    let file_ids: Vec<i64> = {
        let sep = native_separator_of(&path);
        let prefix = format!("{}{}", path.trim_end_matches(sep), sep);
        let path_hi = path_prefix_upper(&prefix);
        let mut stmt = conn.prepare("SELECT id FROM files WHERE path >= ?1 AND (?2 IS NULL OR path < ?2)")?;
        let rows = stmt.query_map(params![prefix, path_hi], |row| row.get(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };
```

Degenerate roots are safe: `"/"` → prefix `"/"`, `"C:\"` → `"C:\"` (trim then re-append is identity). No `files.path` row ever equals a root path (roots are directories), so requiring the separator loses nothing legitimate.

- [ ] **Step 4: Run the new tests, verify they PASS; run the full module**

Run: `cargo test -p athenaeum-core name_prefix_sibling`
Expected: 4 passed.
Run: `cargo test -p athenaeum-core operations`
Expected: all pass (the pre-existing wildcard-sibling and lineage tests must stay green).

- [ ] **Step 5: Decide the read-only helper (`scan_root_prefix_predicate`, ~lines 87-104)**

This helper drops the separator too, and `api/scan_roots.rs:1468-1469` calls that omission deliberate — but no reason is recorded at the helper. Its only consumers are duplicate-detection inclusion filters (`find_duplicate_groups`, `rebuild_duplicate_groups_cache`), so the consequence is over-inclusion (a sibling root with `find_duplicates = 0` leaks into duplicate detection), not destruction.

Check first: `grep -n "scan_root_prefix_predicate" crates/athenaeum-core/src/ -r` and read any test pinning lax behavior. If nothing pins it: make it strict with the same three-line pattern inside the `for root in roots` loop (build `prefix` per root, bind `prefix` instead of `root`), and update the comment at `api/scan_roots.rs:1468-1469` to drop the "deliberately drops" clause. If a test pins the lax behavior with a stated reason: leave the code, replace "deliberately drops" with that reason at the helper's doc.

Run: `cargo test -p athenaeum-core duplicates`
Expected: all pass.

- [ ] **Step 6: Pin the exact upper-bound boundary in the overview tests (#10)**

In `crates/athenaeum-core/src/api/scan_roots.rs` `overview_tests`: the existing exclusion fixtures sit one step away from the bound in each direction. Add one row at exactly the successor character per arm — the value where an off-by-one in `path_prefix_upper` or a `<=` typo would show:

- In `overview_counts_files_and_bytes_per_root`: `insert_file` a row at `<root>0/x.fits` (for root `/data/astro`: `/data/astro0/x.fits`) and assert the root's `file_count`/`total_bytes` are unchanged.
- In the Windows-arm test (`overview_covers_windows_separator_roots` or its current name): add `C:\Astro]x\f.fits` and assert unchanged totals.

Run: `cargo test -p athenaeum-core overview`
Expected: all pass (these are pinning tests — GREEN on arrival, they guard the bound).

- [ ] **Step 7: Format, gate, commit**

Run: `rustfmt --edition 2021 crates/athenaeum-core/src/db/operations.rs crates/athenaeum-core/src/api/scan_roots.rs`
Run: `cargo test -p athenaeum-core`
Expected: full suite green.

```bash
git add crates/athenaeum-core/src/db/operations.rs crates/athenaeum-core/src/api/scan_roots.rs
git commit -m "fix(db): separator-strict byte-range prefixes — deleting or scanning a root no longer touches name-prefix sibling roots"
```

---

### Task 2: `calibration.library_dir` durability (⭐#24a backend + #8 mitigation + role-copy drift)

The audit found the escalated "orphaned library key" is NOT reachable via plain root deletion (`guard_against_special_root_deletion` blocks it) — but IS reachable via `relink_scan_root` (new to this cycle: relink rewrites `files.path` + `scan_roots.path` and leaves the settings key at the old path) and via the switch TOCTOU window (old root purged, delegate fails, key names the removed folder). Fix both; also fix stale "File Manager" copy that predates the Folders tab.

**Files:**
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs` (`relink_scan_root` ~line 1061-1071; `switch_calibration_library_dir` ~lines 626-658 + doc comment ~607-613; guard copy ~lines 806, 819; tests near `switch_library_tests` ~line 2100+)
- Modify: `crates/athenaeum-core/src/api/masters.rs` (~line 520, copy only)

**Interfaces:**
- Consumes: `resolve_calibration_library_dir(&Connection) -> Result<Option<String>, _>` (`api/scan_roots.rs:396`), `ctx.settings.persist_setting(&Connection, key, value)`, `crate::settings::keys::CALIBRATION_LIBRARY_DIR` (`settings/mod.rs:125`; value `""` = explicitly cleared, per the resolver's `Some("")` contract).
- Produces: no signature changes.

- [ ] **Step 1: Write the failing relink test (RED)**

Append near `switch_library_tests` in `api/scan_roots.rs`, mirroring that module's `ServiceContext` + `PathPolicy` fixture style (relink does not require the OLD path to exist on disk — that is its purpose):

```rust
#[test]
fn relink_scan_root_carries_calibration_library_key() {
    let ctx = ServiceContext::new_for_tests();
    let policy = /* same PathPolicy value switch_library_tests passes */;
    let root_id: i64;
    {
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        conn.execute("INSERT INTO scan_roots (path) VALUES ('/gone/root')", []).unwrap();
        root_id = conn
            .query_row("SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
            .unwrap();
        crate::db::set_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR, "/gone/root/masters").unwrap();
    }
    let newdir = tempfile::tempdir().unwrap();

    relink_scan_root(&ctx, root_id, newdir.path().to_string_lossy().to_string(), &policy).unwrap();

    let canon = newdir.path().canonicalize().unwrap();
    let db = db(&ctx).unwrap();
    let conn = db.conn();
    let got = crate::db::get_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR).unwrap();
    assert_eq!(
        got.as_deref(),
        Some(format!("{}/masters", canon.display()).as_str()),
        "library key must follow the relinked covering root"
    );
}

#[test]
fn relink_scan_root_leaves_unrelated_calibration_library_key_alone() {
    // Name-prefix sibling of the relinked root: /gone/rootX is NOT covered
    // by /gone/root — the key must not move.
    let ctx = ServiceContext::new_for_tests();
    let policy = /* same as above */;
    let root_id: i64;
    {
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        conn.execute("INSERT INTO scan_roots (path) VALUES ('/gone/root')", []).unwrap();
        root_id = conn
            .query_row("SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
            .unwrap();
        crate::db::set_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR, "/gone/rootX/masters").unwrap();
    }
    let newdir = tempfile::tempdir().unwrap();

    relink_scan_root(&ctx, root_id, newdir.path().to_string_lossy().to_string(), &policy).unwrap();

    let db = db(&ctx).unwrap();
    let conn = db.conn();
    let got = crate::db::get_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR).unwrap();
    assert_eq!(got.as_deref(), Some("/gone/rootX/masters"));
}
```

- [ ] **Step 2: Run, verify the first test FAILS**

Run: `cargo test -p athenaeum-core carries_calibration_library_key`
Expected: first test FAILS (key still `/gone/root/masters`); second PASSES (nothing moves it today either).

- [ ] **Step 3: Implement relink-follows-root**

In `relink_scan_root`, inside the `if result.files_orphaned == 0 || result.files_matched > 0` block, immediately after the `"updated scan root path"` `info!` (~line 1070):

```rust
        // The calibration library is the only role stored as a SECOND copy
        // of a path (settings key) rather than as the root row itself — a
        // relink that moves the covering root must move the key too, or it
        // orphans: masters keep landing at the old path and the Folders
        // rail loses the covered-library row.
        if let Some(dir) = resolve_calibration_library_dir(&conn)? {
            let sep = if old_path.starts_with('/') { '/' } else { '\\' };
            let old_prefix = format!("{}{}", old_path.trim_end_matches(sep), sep);
            if dir == old_path || dir.starts_with(&old_prefix) {
                let moved = format!("{}{}", new_path, &dir[old_path.len()..]);
                ctx.settings
                    .persist_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR, &moved)
                    .map_err(|e| {
                        tracing::error!(root_id, error = %e, "failed to move calibration library key with relinked root");
                        ApiError::Internal(format!("Relinked, but failed to move the Calibration Library setting: {e}"))
                    })?;
                tracing::info!(root_id, src = %dir, dest = %moved, "calibration library dir followed relinked root");
            }
        }
```

(`dir == old_path` covers relinking a dedicated library root itself; the byte-slice is char-boundary-safe because `starts_with` guarantees the prefix. If `resolve_calibration_library_dir`'s error type isn't already `ApiError`, adapt with the surrounding function's existing conversion style — never silently.)

- [ ] **Step 4: Run, verify both tests PASS**

Run: `cargo test -p athenaeum-core carries_calibration_library_key`
Expected: 2 passed. Also run `cargo test -p athenaeum-core relink` — pre-existing relink tests stay green.

- [ ] **Step 5: Switch-failure demote (closes the #8 residual's confusing half)**

In `switch_calibration_library_dir` (~lines 640-658): track whether the destructive step ran, and on delegate failure demote the key to `""` (the "role unassigned" state every screen renders) instead of leaving it naming a purged folder. **Gate on `removed_old_root`** — when no root was deleted (covered library, delegate-only path), a failure must NOT clear a still-working key.

```rust
    let mut removed_old_root = false;
    if let Some(old) = old_root {
        if canonical_or_raw(&old.path) != new_path {
            // ... existing preflight + delete_scan_root block unchanged ...
            removed_old_root = true;
        }
    }

    set_calibration_library_dir(ctx, new_path.to_string_lossy().to_string(), policy).map_err(|e| {
        if removed_old_root {
            // The old root is already purged; a key naming a removed folder
            // is the one state no screen can explain. Demote to "role
            // unassigned" — designating again is the documented recovery.
            match db(ctx) {
                Ok(db) => {
                    let conn = db.conn();
                    if let Err(e2) = ctx.settings.persist_setting(
                        &conn,
                        crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                        "",
                    ) {
                        tracing::error!(error = %e2, "failed to clear stale calibration library key after failed switch");
                    }
                }
                Err(e2) => {
                    tracing::error!(error = %e2, "no db handle to clear stale calibration library key after failed switch");
                }
            }
            tracing::error!(error = %e, "library switch failed after old root removal — role left unassigned");
        }
        e
    })
```

Update the doc comment (~lines 607-613): the residual TOCTOU paragraph now ends in "the key is cleared and the role reads unassigned; designate again to recover" instead of "the setting still names the removed folder".

- [ ] **Step 6: Test the demote path (deterministic TOCTOU fixture)**

The only deterministic delegate-fails-after-delete lever is the documented two-library TOCTOU state (`check_special_root_uniqueness` is the one check the preflight lacks — rollup #5, deliberately unfixed in this wave):

```rust
#[test]
fn switch_failure_after_delete_demotes_key_instead_of_orphaning() {
    // TOCTOU-violated state: TWO calibration_library kind rows. Preflight
    // passes (it has no uniqueness arm), the old root is purged, then the
    // delegate's check_special_root_uniqueness fails — the key must demote
    // to "" (explicitly cleared), never keep naming the removed folder.
    let ctx = ServiceContext::new_for_tests();
    let policy = /* same as switch_library_tests */;
    let olddir = tempfile::tempdir().unwrap();
    let stray = tempfile::tempdir().unwrap();
    let newdir = tempfile::tempdir().unwrap();
    {
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO scan_roots (path, kind) VALUES (?1, 'calibration_library'), (?2, 'calibration_library')",
            rusqlite::params![
                olddir.path().canonicalize().unwrap().to_string_lossy(),
                stray.path().canonicalize().unwrap().to_string_lossy()
            ],
        )
        .unwrap();
        crate::db::set_setting(
            &conn,
            crate::settings::keys::CALIBRATION_LIBRARY_DIR,
            &olddir.path().canonicalize().unwrap().to_string_lossy(),
        )
        .unwrap();
    }

    let err = switch_calibration_library_dir(
        &ctx,
        newdir.path().to_string_lossy().to_string(),
        &policy,
    );

    assert!(err.is_err(), "delegate uniqueness check must reject the stray second library root");
    let db = db(&ctx).unwrap();
    let conn = db.conn();
    let got = crate::db::get_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR).unwrap();
    assert_eq!(got.as_deref(), Some(""), "key must be demoted, not orphaned");
}
```

Caveat: if `get_calibration_library_root` itself refuses the two-row state (errors before the delete), this fixture can't reach the demote path — then drop the test, note "demote path covered by review; no deterministic fixture" in the test module, and keep Step 5's code.

Run: `cargo test -p athenaeum-core switch`
Expected: new test passes (or is documented-dropped per the caveat); pre-existing switch tests green.

- [ ] **Step 7: Fix stale role-copy ("File Manager" → "File Manager → Folders")**

Three user-facing strings predate the Folders tab (surface renamed in `f1faa594`):
- `api/scan_roots.rs` ~line 806 and ~819 (`guard_against_special_root_deletion` messages): "…in File Manager first…" → "…in File Manager → Folders first…"
- `api/masters.rs` ~line 520 (`check_library_dir_exists`): "File Manager → Calibration Folder" → "File Manager → Folders"

Then `grep -rn "File Manager" crates/athenaeum-core/src --include=*.rs` — update any test pinning these exact strings.

- [ ] **Step 8: Format, gate, commit**

Run: `rustfmt --edition 2021 crates/athenaeum-core/src/api/scan_roots.rs crates/athenaeum-core/src/api/masters.rs`
Run: `cargo test -p athenaeum-core`
Expected: full suite green.

```bash
git add crates/athenaeum-core/src/api/scan_roots.rs crates/athenaeum-core/src/api/masters.rs
git commit -m "fix(folders): calibration library key follows relinked root; failed switch demotes role instead of orphaning the key; Folders-tab copy"
```

---

### Task 3: Frontend safety fixes (#14 unguarded RoleKind cast, #15 off-palette checkbox, #23d wrong-cause sticky banner)

**Files:**
- Modify: `src/components/folders/roleMeta.ts` (add guard)
- Modify: `src/components/folders/FoldersTab.tsx` (~lines 403, 424, 416-418, 501-505)
- Modify: `src/components/folders/FolderRail.tsx` (~lines 80-82)
- Modify: `src/components/folders/SwitchRow.tsx` (line 18)

**Interfaces:**
- Produces: `isRoleKind(k: string): k is RoleKind` exported from `roleMeta.ts` — Task 4 and follow-up items reuse it.
- Consumes: `clearRootsError()` from `useScanRootsWithAvailability` (already destructured in `FoldersTab`), the alert affordance the remove-root path already uses.

- [ ] **Step 1: Add the type guard**

In `src/components/folders/roleMeta.ts`, next to `ROLE_META`:

```ts
/** Narrowing guard: DB `scan_roots.kind` is an open string — a newer build's
 *  kind (version downgrade) must fall back to the generic monitored UI, never
 *  reach `ROLE_META[kind]` undefined (white screen — there is no ErrorBoundary). */
export const isRoleKind = (k: string): k is RoleKind => k in ROLE_META;
```

- [ ] **Step 2: Guard both cast sites**

`FoldersTab.tsx` ~line 403: route unknown kinds to the safe generic inspector —

```ts
if (root.kind === 'normal' || !isRoleKind(root.kind)) {
```

(the `~424` `root.kind as RoleKind` cast below it is now only reachable when `isRoleKind` held — replace the cast with the narrowed `root.kind`).

`FolderRail.tsx` ~lines 80-82: unknown kinds must appear in the rail instead of vanishing —

```ts
const monitored = roots.filter((r) => !isRoleKind(r.kind) || r.kind === 'normal');
const roleRoots = new Map<RoleKind, ScanRootInfo>(
  roots.filter((r) => isRoleKind(r.kind) && r.kind !== 'normal').map((r) => [r.kind as RoleKind, r]),
);
```

(Adapt to the file's actual variable names; if `'normal'` is not a `ROLE_META` key, the `!== 'normal'` arms are redundant — drop them. Duplicate-kind Map shadowing stays a deferred item, rollup #17c.)

- [ ] **Step 3: Fix the checkbox tint**

`SwitchRow.tsx` line 18 — the `text-accent`/`border-border`/`focus:ring-*` utilities are inert on a native checkbox without `@tailwindcss/forms` (not installed; `tailwind.config.js` has `plugins: []`). Use the house pattern (9 live uses, e.g. `archive/RestoreDialog.tsx:120`):

```tsx
className="mt-1 w-4 h-4 accent-accent"
```

- [ ] **Step 4: Fix toggle-failure error surfacing**

`FoldersTab.tsx` ~lines 416-418: the toggle handlers' `catch` currently `console.error`s while the hook's `setError` has already painted the global "Error loading folders" banner (wrong noun, wrong cause, no dismiss). Mirror the remove-root path (~line 282): clear the hook error and surface the failure through the same alert affordance the remove path uses, with the file's existing error-text idiom:

```ts
} catch (e) {
  clearRootsError();
  const msg = typeof e === 'string' ? e : String(e);
  showAlert('Could not change setting', msg); // same affordance as the remove-root path
}
```

And give the banner (~lines 501-505) a dismiss control:

```tsx
<button
  onClick={clearRootsError}
  aria-label="Dismiss"
  className="ml-2 p-1 rounded hover:bg-surface-hover text-content-muted"
>
  <X size={14} />
</button>
```

(`X` from `lucide-react`; if `FoldersTab` has no `showAlert`-style helper, reuse exactly whatever the remove path uses — do not invent a new toast.)

- [ ] **Step 5: Gate and commit**

Run: `npx tsc --noEmit`
Expected: exit 0.
Run: `npm run build`
Expected: green.

```bash
git add src/components/folders/roleMeta.ts src/components/folders/FoldersTab.tsx src/components/folders/FolderRail.tsx src/components/folders/SwitchRow.tsx
git commit -m "fix(folders): guard unknown scan-root kinds, native-checkbox accent tint, honest dismissible toggle-failure errors"
```

---

### Task 4: Cosmetic sweep (zero-behavior-risk batch, one commit)

All items verified against current line numbers by the 2026-07-30 audit; each is independently safe.

**Files:**
- Modify: `src/components/folders/FolderRail.tsx`, `MonitoredInspector.tsx`, `ArchiveInspector.tsx`, `RoleInspector.tsx`, `FoldersTab.tsx`
- Modify: `src/pages/FileManager.tsx`
- Modify: `src/pages/Settings.tsx` (one aria-label)
- Delete: `src/components/DirectoryTree.tsx`
- Modify: `crates/athenaeum-tauri/src/commands/scan_roots.rs` (lines 16, 121, 135)

**Interfaces:** none — labels, comments, a11y attributes, import hygiene, dead code.

- [ ] **Step 1: Apply the sweep table**

| Rollup # | Site | Change |
| ---- | ---- | ---- |
| 4 | `commands/scan_roots.rs:121,135` | Move the two mid-file `pub use` (`FolderCandidateVerdict`, `FolderOverview`) into the top-of-file `pub use athenaeum_core::api::scan_roots::{…}` group at line 16; delete lines 121/135. |
| 9 | `ArchiveInspector.tsx:75` | `label="total size"` → `label="frame-set zips"` — the number deliberately excludes calibration-originals zips (doc'd backend decision); the label must scope itself like its sibling chip. |
| 17a | `FolderRail.tsx:109-114,130-135` | Hoist `const id = root.id; if (id == null) return null;` once per row renderer; drop every `root.id!`. |
| 17b | `FolderRail.tsx:38` | `React.ComponentType<…>` (UMD-global type ref, no import) → `LucideIcon` imported from `./roleMeta`. |
| 20a | `MonitoredInspector.tsx:42,147,149,164` | Split `missingRootId` into `const showMissing = !offline && missingCount > 0 && root.id != null;` (gate) + pass `root.id` (prop). |
| 20d | `MonitoredInspector.tsx:100` | Offline-banner Relink: `disabled={relinking}` → `disabled={relinking \|\| isScanning}` (parity with header twin at :81). |
| 20e | `MonitoredInspector.tsx:168-169` | Drop the stray `mt-2` on the errors card; put `space-y-2` on the Section body instead. |
| 20g | `MonitoredInspector.tsx:151,170` | Add `aria-expanded={missingOpen}` / `aria-expanded={errorsOpen}` (+ `aria-controls` ids) to the two disclosure buttons. |
| 22d | `ArchiveInspector.tsx:56` | Literal `★` → `<Star size={10} fill="currentColor" />` (matches sibling button :57 and `FolderRail.tsx:193`). |
| 22e | `MonitoredInspector.tsx:71`, `RoleInspector.tsx:55`, `ArchiveInspector.tsx:63,101`, `Settings.tsx:1307` | Add `aria-label="Reveal in file manager"` to the icon-only reveal buttons (title stays). |
| 23b | `FoldersTab.tsx:205-222` | Remove non-load-bearing `coveredCalibrationDir` from the reconcile effect deps; reword the comment: the covered case is preserved by the absence of a matching root, not by consulting the dir. |
| 25a | `FileManager.tsx:49-53` | Rewrite the token comment — it describes the retired monotonic counter. New text: "Raise/consume token: the parent raises it to 1, FoldersTab selects the role and calls onSyncIncomingHandled, which lowers it to 0 — the 0 also re-arms the child's latch so a repeat deep-link reusing 1 reads as new. Do NOT make this monotonic again: that reintroduces the replay bug fixed in 91da41bf." |
| 25b | `FileManager.tsx:126` | Subtitle → `Manage folders, roles and archive destinations; browse FITS/XISF files and metadata`. |
| 25c | `src/components/DirectoryTree.tsx` | `git rm` — zero imports repo-wide (dead since the dual-pane cycle); its `:207` copy names the removed tab. Also fix `useTauri.ts:14`'s "(monitored directories)" comment while there. |

- [ ] **Step 2: Gate**

Run: `npx tsc --noEmit` — expected exit 0 (items 17b/25c touch types/imports).
Run: `npm run build` — expected green.
Run: `cargo build -p athenaeum-tauri` — expected green (item 4).

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "chore(folders): deferred-minors sweep — a11y labels, honest chip label, dead DirectoryTree removal, comment/subtitle truth, import hygiene"
```

---

### Task 5: Full gates + ledger/rollup sync

**Files:**
- Modify: `.superpowers/sdd/progress.md` (append one ledger line)
- Verify: `.superpowers/sdd/folders-minors-rollup.md` (already rewritten with audit verdicts on 2026-07-30 — confirm the four fix-wave sections now read FIXED with the commit hashes from Tasks 1-4)

- [ ] **Step 1: Run all release-grade gates**

```bash
cargo build --workspace
cargo test -p athenaeum-core
npx tsc --noEmit
npm run build
```

Expected: all green. Any failure → fix within the task that caused it before proceeding.

- [ ] **Step 2: Update the SDD ledger**

Append to the FOLDERS SCREEN REDESIGN section of `.superpowers/sdd/progress.md`:

```
- FOLDERS-MINORS-FIX-WAVE: complete (4 commits: db byte-range separator fix ×3 sites + tests; library-key relink-follow + switch demote + copy; frontend safety ×3; cosmetic sweep ×15). Audit + triage: folders-minors-rollup.md (rewritten with verdicts 2026-07-30). Follow-up backlog stays in the rollup; #8 real atomicity, house-wide modal a11y, formatBytes consolidation remain deferred.
```

- [ ] **Step 3: Stamp the fix commits into the rollup**

In `.superpowers/sdd/folders-minors-rollup.md`, fill each `fixed in <commit>` placeholder in the "FIX NOW" section with the real hashes from Tasks 1-4.

```bash
git add .superpowers/sdd/progress.md .superpowers/sdd/folders-minors-rollup.md
git commit -m "docs(sdd): folders minors fix-wave ledger + rollup verdict stamps"
```

---

## Explicitly OUT of this plan (triaged follow-up / keep-deferred)

Recorded with per-item verdicts, proposed fixes and efforts in `.superpowers/sdd/folders-minors-rollup.md` (rewritten 2026-07-30). Highlights: `formatBytes` consolidation is repo-wide (8 copies, missing TB tier) — its own small cycle; backend observability pass (#1/#2/#3/#5/#7) is one S-sized core-only commit when convenient; switch atomicity (#8) stays deferred (M–L refactor through `add_scan_root`, blast radius beyond Folders — the Task 2 demote removes its confusing consequence); house-wide modal a11y and scrim tokens are app-wide passes, not Folders items.
