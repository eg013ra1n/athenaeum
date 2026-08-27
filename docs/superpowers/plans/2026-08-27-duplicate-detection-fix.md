# Duplicate Detection Fix — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Duplicates view group files by an identity that survives copying, so the 2 750 real duplicate groups (170.5 GiB) currently invisible on a production catalog become visible — without ever grouping two different frames.

**Architecture:** Replace the `use_content_hash: bool` that threads through duplicate detection with a `DuplicateKey` enum owning the SQL for each key. The cheap key stops being `files.metadata_hash` (`size + mtime + filename`, which encodes a property of the *copy*) and becomes `(fits_header.header_fingerprint, files.size)` — already computed at scan time for relinking, already indexed, 100 % populated, mtime-independent — restricted to raw sub-frames. Masters and processed files are excluded from that key — their headers are shared by construction — and get their own path in Task 7: the same header fingerprint shortlists them (381 files down to 61), and a full-file hash in a new `files.strong_hash` column decides which of the shortlist are really byte-identical.

**Tech Stack:** Rust (rusqlite, SQLite), React/TypeScript, Tailwind design tokens.

**Spec:** `docs/superpowers/specs/2026-08-27-duplicate-detection-design.md`

## Global Constraints

- **Two backends in sync.** Anything touching a Tauri command needs the matching Axum route in the same change. Real logic lives in `athenaeum-core`.
- **Never swallow errors.** Log to `tracing` before returning.
- `anyhow::Result` inside core; `.map_err(|e| e.to_string())` at the command boundary.
- **Design tokens, not raw colors** in any frontend change (`bg-surface`, `text-content-muted`, …).
- **Message style:** short stable phrase, data in snake_case fields — `debug!(groups = 2750, "duplicate groups computed")`, never interpolated prose.
- Real gates are `cargo build --workspace`, `cargo test -p athenaeum-core`, `npx tsc --noEmit`. Clippy is not a gate.
- Format only the lines you add: `schema.rs` and `operations.rs` are **not** rustfmt-clean at HEAD, so `rustfmt` on the whole file produces ~900 lines of unrelated churn. Match surrounding style by hand.
- **The header key never decides a master.** Measured 0/30 precision; spec §2.4 cause 3 proves the ceiling is structural, and §2.5 proves no sampling scheme fixes it (detection tracks coverage linearly — one changed Float32 pixel is 4 bytes in 77 MiB). Masters are *shortlisted* by header and *decided* by a full-file hash (Task 7). Do not "improve" the header key to try to decide them.
- **The header key joins two tables that have NO `UNIQUE(file_id)`.** `frames` and `fits_header` are 1:1 with `files` by convention only — `scanner/mod.rs:1423` says so in a comment and works around it with DELETE-then-INSERT, while `calibration_library/headers.rs`, `db/repair.rs` and `relinking/mod.rs` all do a bare INSERT. Every aggregate over the joined shape must therefore be fan-out-proof (`COUNT(DISTINCT f.id)`, deduped id/path lists). A duplicated child row must never be able to present a lone file as a group of two.

---

### Task 1: `DuplicateKey` enum and the header-identity query

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations.rs` (add enum after `scan_root_prefix_predicate`, ~line 138; rewrite `find_duplicate_groups`, 1885-1949)
- Modify: `crates/athenaeum-core/src/db/mod.rs` (re-export `DuplicateKey`)
- Test: `crates/athenaeum-core/src/db/operations.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `scan_root_prefix_predicate(column, roots) -> (String, Vec<Value>)` and `enrich_duplicate_groups(conn, &mut [DuplicateGroup])`, both already in this file. `SavepointGuard` is `pub(crate)` in this file (not needed here, noted for Task 2).
- Produces: `pub enum DuplicateKey { Header, Content }` with `from_setting(bool) -> Self`, `hash_type(self) -> &'static str`, and private `hash_expr(self, files_alias: &str) -> String` / `joins(self, files_alias: &str) -> String` / `eligibility(self) -> &'static str` / `hash_is_usable(self, files_alias: &str) -> String`; plus `find_duplicate_groups(conn: &Connection, key: DuplicateKey) -> Result<Vec<DuplicateGroup>>`. Tasks 2-4 use these exact names. **Every accessor takes the `files` alias**, because `rebuild_duplicate_groups_cache` (Task 2) runs one query aliased `f` and another aliased `files`. **Task 7 adds a third variant `Master`** — write every `match self` here as an exhaustive match over the two variants (no `_ =>` arm), so adding it there produces compiler errors at each place that must be extended rather than a silent wrong default.

- [ ] **Step 1: Write the failing tests**

Add this helper and these five tests to the `#[cfg(test)] mod tests` block in `operations.rs`:

```rust
/// files + frames + fits_header rows for one duplicate-detection test file.
/// `header` becomes the stored blob AND drives the fingerprint, so two files
/// sharing a `header` string share a fingerprint — which is exactly the
/// production relation between two copies of one exposure.
fn seed_dup_file(
    conn: &Connection,
    id: i64,
    path: &str,
    modified_at: &str,
    header: &str,
    imagetyp: &str,
    is_master: i64,
) {
    let filename = path.rsplit('/').next().unwrap();
    let meta = crate::duplicates::compute_metadata_hash(
        100,
        &chrono::DateTime::parse_from_rfc3339(modified_at)
            .unwrap()
            .with_timezone(&chrono::Utc),
        filename,
    );
    conn.execute(
        "INSERT INTO files (id, path, filename, size, modified_at, format, metadata_hash)
         VALUES (?1, ?2, ?3, 100, ?4, 'FITS', ?5)",
        params![id, path, filename, modified_at, meta],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (?1, ?1, ?2, ?3)",
        params![id, imagetyp, is_master],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, ?2, ?3)",
        params![id, header, crate::fingerprint::compute_header_fingerprint(header)],
    )
    .unwrap();
}

/// A scan root that opts into duplicate detection. Every test below needs one:
/// the query gates on `find_duplicates = 1`.
fn seed_dup_root(conn: &Connection, path: &str, find_duplicates: i64) {
    conn.execute(
        "INSERT INTO scan_roots (path, find_duplicates) VALUES (?1, ?2)",
        params![path, find_duplicates],
    )
    .unwrap();
}

/// Two byte-identical copies of one frame whose mtimes drifted — the shape of
/// production calibration set 628, where an exFAT hop rounded one copy's mtime
/// up to the next even whole second. The header key must see one group.
#[test]
fn header_key_groups_copies_whose_mtime_drifted() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    seed_dup_root(&conn, "/vol", 1);

    seed_dup_file(&conn, 1, "/vol/a/flat_0000.fits", "2024-10-05T04:21:46+00:00",
                  "HDR-A", "Flat", 0);
    seed_dup_file(&conn, 2, "/vol/b/flat_0000.fits", "2024-10-05T04:21:44.307+00:00",
                  "HDR-A", "Flat", 0);

    let groups = find_duplicate_groups(&conn, DuplicateKey::Header).unwrap();
    assert_eq!(groups.len(), 1, "one group expected, got {groups:#?}");
    assert_eq!(groups[0].file_count, 2);
    let mut ids = groups[0].file_ids.clone();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2]);
}

/// Masters never come back from the header key. Measured 0/30 precision:
/// `Pane_2_Sii.xisf` and `Pane_2_Ha.xisf` carry byte-identical FITS keywords
/// (spec §2.4), so grouping them would offer one filter as a copy of another.
#[test]
fn header_key_excludes_masters_and_processed_frames() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    seed_dup_root(&conn, "/vol", 1);

    // is_master = 1 with a raw imagetyp — the shape a bare imagetyp check
    // would let through (production holds 2 such Darks and 1 Flat).
    seed_dup_file(&conn, 1, "/vol/a/m.xisf", "2024-01-01T00:00:00+00:00",
                  "HDR-M", "Dark", 1);
    seed_dup_file(&conn, 2, "/vol/b/m.xisf", "2024-01-01T00:00:01+00:00",
                  "HDR-M", "Dark", 1);
    // A processed imagetyp with is_master = 0 — the shape a bare is_master
    // check would let through (production holds 245 MasterLight rows).
    seed_dup_file(&conn, 3, "/vol/a/p.xisf", "2024-01-01T00:00:00+00:00",
                  "HDR-P", "MasterLight", 0);
    seed_dup_file(&conn, 4, "/vol/b/p.xisf", "2024-01-01T00:00:01+00:00",
                  "HDR-P", "MasterLight", 0);

    let groups = find_duplicate_groups(&conn, DuplicateKey::Header).unwrap();
    assert!(groups.is_empty(), "masters must not be offered, got {groups:#?}");
}

/// A file the scanner gave no header row, or gave an empty one, is simply not
/// grouped — a miss, never a false positive. Covers the three scanner branches
/// that insert no `fits_header` row and sync-ingest's empty row. Without the
/// `<> ''` guard every header-less file shares one fingerprint (the hash of
/// the empty string) and they would all group together.
#[test]
fn header_key_skips_files_without_a_usable_header() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    seed_dup_root(&conn, "/vol", 1);

    seed_dup_file(&conn, 1, "/vol/a/x.fits", "2024-01-01T00:00:00+00:00",
                  "", "Light", 0);
    seed_dup_file(&conn, 2, "/vol/b/x.fits", "2024-01-01T00:00:01+00:00",
                  "", "Light", 0);
    // id 3 gets files + frames rows but no fits_header row at all.
    conn.execute(
        "INSERT INTO files (id, path, filename, size, modified_at, format)
         VALUES (3, '/vol/c/x.fits', 'x.fits', 100, '2024-01-01T00:00:02+00:00', 'FITS')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (3, 3, 'Light', 0)",
        [],
    )
    .unwrap();

    // The empty-blob rows must not group with each other, and the row with no
    // header at all must not appear.
    let groups = find_duplicate_groups(&conn, DuplicateKey::Header).unwrap();
    assert!(groups.is_empty(), "unusable headers must not group, got {groups:#?}");
}

/// `frames` and `fits_header` have no `UNIQUE(file_id)` — `scanner/mod.rs:1423`
/// says so and works around it, while three other call sites do a bare INSERT.
/// A duplicated child row must not fan the join out into a phantom group: ONE
/// file is never a duplicate of itself.
#[test]
fn header_key_survives_a_duplicated_child_row() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    seed_dup_root(&conn, "/vol", 1);

    seed_dup_file(&conn, 1, "/vol/a/only.fits", "2024-01-01T00:00:00+00:00",
                  "HDR-A", "Light", 0);
    // A second header row for the same file — permitted by the schema.
    conn.execute(
        "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (1, ?1, ?2)",
        params!["HDR-A", crate::fingerprint::compute_header_fingerprint("HDR-A")],
    )
    .unwrap();

    let groups = find_duplicate_groups(&conn, DuplicateKey::Header).unwrap();
    assert!(
        groups.is_empty(),
        "a single file must never be its own duplicate, got {groups:#?}"
    );

    // And with a genuine second file, the group is still exactly two files.
    seed_dup_file(&conn, 2, "/vol/b/only.fits", "2024-01-01T00:00:05+00:00",
                  "HDR-A", "Light", 0);
    let groups = find_duplicate_groups(&conn, DuplicateKey::Header).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].file_count, 2, "fan-out must not inflate the count");
    assert_eq!(groups[0].file_ids.len(), 2);
    assert_eq!(groups[0].file_paths.len(), 2);
}

/// The two gates the old key already applied still apply: a black-holed file
/// leaves its group, and a file outside every `find_duplicates = 1` root is
/// never considered.
#[test]
fn header_key_still_honours_black_hole_and_scan_root_gating() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    seed_dup_root(&conn, "/vol", 1);
    seed_dup_root(&conn, "/other", 0);

    seed_dup_file(&conn, 1, "/vol/a/f.fits", "2024-01-01T00:00:00+00:00",
                  "HDR-A", "Flat", 0);
    seed_dup_file(&conn, 2, "/vol/b/f.fits", "2024-01-01T00:00:01+00:00",
                  "HDR-A", "Flat", 0);
    seed_dup_file(&conn, 3, "/other/f.fits", "2024-01-01T00:00:02+00:00",
                  "HDR-A", "Flat", 0);

    let groups = find_duplicate_groups(&conn, DuplicateKey::Header).unwrap();
    assert_eq!(groups.len(), 1, "the /other root has find_duplicates = 0");
    assert_eq!(groups[0].file_count, 2);

    conn.execute(
        "INSERT INTO black_hole (file_id, from_where, moved_at, original_path)
         VALUES (2, 'test', '2024-01-01T00:00:00+00:00', '/vol/b/f.fits')",
        [],
    )
    .unwrap();
    assert!(
        find_duplicate_groups(&conn, DuplicateKey::Header).unwrap().is_empty(),
        "one survivor is not a duplicate group"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p athenaeum-core --lib db::operations::tests::header_key -- --nocapture`
Expected: FAIL to compile — `cannot find type DuplicateKey in this scope`.

- [ ] **Step 3: Add the enum**

Insert into `crates/athenaeum-core/src/db/operations.rs` directly after `scan_root_prefix_predicate` ends (~line 138):

```rust
/// Which identity two files must share before the Duplicates view calls them
/// copies of each other.
///
/// This replaces a `use_content_hash: bool` that had come to mean three
/// different things (which column to group on, which `duplicate_groups.hash_type`
/// to write, which files are eligible at all), and it replaces
/// `files.metadata_hash` as the cheap key.
///
/// `metadata_hash` is `xxh3(size + modified_at + filename)` — every term but
/// one is a property of the FRAME, and `modified_at` is a property of the
/// COPY. Copying is free to change it and routinely does: on the owner's
/// catalog 2 189 of 2 763 duplicate candidates carry FAT/exFAT's two-second
/// timestamp granularity on one side (a Windows capture PC to a Mac via a USB
/// volume, the normal path for astro data), and another 548 were copied
/// without `-p` so their mtime is the copy time. The measured result: the
/// Duplicates view returned ZERO groups on a 41 893-file catalog holding
/// 2 750 real ones. See `specs/2026-08-27-duplicate-detection-design.md` §2.1.
///
/// `Header` is `fits_header.header_fingerprint` — `xxh3` of the stored header
/// blob, already written at scan time for relinking, already indexed, and
/// independent of mtime by construction.
///
/// Every SQL accessor takes the caller's `files` alias, because the cache
/// rebuild runs one query aliased `f` and a second aliased `files`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateKey {
    /// Header identity. RAW SUB-FRAMES ONLY — see [`Self::eligibility`].
    Header,
    /// Byte content (`files.content_hash`). Needs the content index, and is
    /// the only key masters and processed files are offered under.
    Content,
}

impl DuplicateKey {
    /// Map the `duplicates.use_content_hash` setting onto a key.
    pub fn from_setting(use_content_hash: bool) -> Self {
        if use_content_hash {
            Self::Content
        } else {
            Self::Header
        }
    }

    /// The value stored in `duplicate_groups.hash_type`. The two keys must
    /// never share one, or a cached group built under one key would be served
    /// for the other.
    pub fn hash_type(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Content => "content",
        }
    }

    /// SQL expression yielding the grouping hash.
    fn hash_expr(self, files_alias: &str) -> String {
        match self {
            Self::Header => "fh.header_fingerprint".to_string(),
            Self::Content => format!("{files_alias}.content_hash"),
        }
    }

    /// Extra joins the expression needs.
    ///
    /// Neither `fits_header` nor `frames` has a `UNIQUE(file_id)`, so these
    /// joins can fan out. Callers must aggregate with `COUNT(DISTINCT …)` and
    /// de-duplicate any concatenated id/path list — see
    /// `find_duplicate_groups`.
    fn joins(self, files_alias: &str) -> String {
        match self {
            Self::Header => format!(
                "JOIN fits_header fh ON fh.file_id = {files_alias}.id \
                 JOIN frames fr ON fr.file_id = {files_alias}.id"
            ),
            Self::Content => String::new(),
        }
    }

    /// Which files this key may consider at all.
    ///
    /// The header key takes raw sub-frames and nothing else. A PixInsight
    /// master carries the FITS keywords of whichever image was the
    /// integration REFERENCE, so `Pane_2_Sii.xisf` genuinely states
    /// `FILTER = 'H'`; and `Pane_2_Sii.xisf` / `Pane_2_Sii_f.xisf` share all
    /// 364 keywords, all 21 XISF properties, geometry and location, differing
    /// only in pixels. Measured precision on that bucket: 0 of 30. No
    /// header-level key of any completeness can separate them — do not widen
    /// this. Spec §2.4.
    ///
    /// Both conditions are needed: `is_master` alone misses the 245
    /// `MasterLight` rows that carry `is_master = 0`, and `imagetyp` alone
    /// misses the production rows that are `Dark`/`Flat` WITH `is_master = 1`.
    ///
    /// An allowlist, not a denylist: an unclassified or newly-introduced
    /// imagetyp is excluded by default, and exclusion is a miss rather than a
    /// deletion. On the owner's catalog this costs nothing measurable — zero
    /// duplicate groups involve a blank imagetyp.
    fn eligibility(self) -> &'static str {
        match self {
            Self::Header => {
                "AND COALESCE(fr.is_master, 0) = 0 \
                 AND fr.imagetyp IN ('Light', 'Flat', 'Dark', 'Bias', 'DarkFlat')"
            }
            Self::Content => "",
        }
    }

    /// Rejects a hash that is present but useless. An empty blob hashes to a
    /// perfectly valid fingerprint that every other empty blob shares, so
    /// `IS NOT NULL` alone would group every header-less file together.
    fn hash_is_usable(self, files_alias: &str) -> String {
        let expr = self.hash_expr(files_alias);
        format!("{expr} IS NOT NULL AND {expr} <> ''")
    }
}
```

- [ ] **Step 4: Rewrite the query**

Replace `find_duplicate_groups` (`operations.rs:1885`) in full:

```rust
/// Group files that are duplicates of each other under `key`.
///
/// Both keys apply the same two gates the original query did: a file in the
/// Black Hole is excluded, and a file must sit under a `scan_roots` row with
/// `find_duplicates = 1`.
///
/// Fan-out safety: the header key joins `fits_header` and `frames`, neither of
/// which has a `UNIQUE(file_id)` (see [`DuplicateKey::joins`]). The count is
/// therefore `COUNT(DISTINCT f.id)` and the concatenated lists are
/// de-duplicated below — SQLite's `GROUP_CONCAT` cannot take both `DISTINCT`
/// and a separator, so the de-duplication happens in Rust. Without it a single
/// file with two header rows would present itself as a group of two and be
/// offered for deletion.
pub fn find_duplicate_groups(
    conn: &Connection,
    key: DuplicateKey,
) -> Result<Vec<DuplicateGroup>> {
    // Scan roots eligible for duplicate detection, fetched once in Rust so
    // the path predicate can be bound as byte-range params per root instead
    // of a per-row SQL-side `LIKE .. || '%'` concat.
    let roots: Vec<String> = {
        let mut stmt = conn.prepare("SELECT path FROM scan_roots WHERE find_duplicates = 1")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    let (root_predicate, root_values) = scan_root_prefix_predicate("f.path", &roots);

    let query = format!(
        "SELECT {hash}, f.size, COUNT(DISTINCT f.id) as count,
                GROUP_CONCAT(f.path, '|') as paths, GROUP_CONCAT(f.id, '|') as ids
         FROM files f
         {joins}
         WHERE {usable}
         AND NOT EXISTS (
             SELECT 1 FROM black_hole bh WHERE bh.file_id = f.id
         )
         {eligibility}
         AND ({roots})
         GROUP BY {hash}, f.size
         HAVING count > 1
         ORDER BY count DESC, f.size DESC",
        hash = key.hash_expr("f"),
        joins = key.joins("f"),
        usable = key.hash_is_usable("f"),
        eligibility = key.eligibility(),
        roots = root_predicate,
    );

    let mut stmt = conn.prepare(&query)?;

    let mut groups: Vec<DuplicateGroup> = stmt
        .query_map(rusqlite::params_from_iter(root_values.iter()), |row| {
            let ids_str: String = row.get(4)?;
            let paths_str: String = row.get(3)?;

            // Zip ids with paths and keep the first occurrence of each id, so
            // a fanned-out join contributes each file exactly once and the
            // two vectors stay index-aligned.
            let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
            let mut file_ids: Vec<i64> = Vec::new();
            let mut file_paths: Vec<String> = Vec::new();
            for (id, path) in ids_str.split('|').zip(paths_str.split('|')) {
                let Ok(id) = id.parse::<i64>() else { continue };
                if seen.insert(id) {
                    file_ids.push(id);
                    file_paths.push(path.to_string());
                }
            }

            Ok(DuplicateGroup {
                id: None,
                size: row.get(1)?,
                // Field name predates the enum: it carries whichever hash
                // `key` selected, a header fingerprint or a content hash. Left
                // as `content_hash` deliberately — it is a serde/ts-rs
                // contract field (`src/types/models.ts::DuplicateGroup`) and
                // no frontend code reads it, so renaming would churn the TS
                // contract for no user-visible gain.
                content_hash: row.get(0)?,
                file_count: row.get(2)?,
                file_paths,
                file_ids,
                files: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    tracing::debug!(key = ?key, groups = groups.len(), "duplicate groups computed");

    enrich_duplicate_groups(conn, &mut groups)?;
    Ok(groups)
}
```

- [ ] **Step 5: Re-export the enum**

In `crates/athenaeum-core/src/db/mod.rs`, add `DuplicateKey` to the existing `pub use operations::{…}` list, beside `find_duplicate_groups`.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p athenaeum-core --lib db::operations::tests::header_key`
Expected: PASS, 5 tests.

- [ ] **Step 7: Fix the existing call sites in this file only**

Run: `cargo test -p athenaeum-core`
Expected: compile errors at every `find_duplicate_groups(&conn, true/false)` and at the cache functions. In THIS task fix only the tests inside `operations.rs`: `true` becomes `DuplicateKey::Content`, `false` becomes `DuplicateKey::Header`. The cache functions and production callers belong to Tasks 2-3 and stay red until then.

- [ ] **Step 8: Commit**

```bash
git add crates/athenaeum-core/src/db/operations.rs crates/athenaeum-core/src/db/mod.rs
git commit -m "fix(duplicates): group by header identity, not by copy mtime

metadata_hash is xxh3(size + modified_at + filename); modified_at is a
property of the copy, not of the frame. On a 41893-file catalog the
Duplicates view returned zero groups while holding 2750 real ones, because
2189 of 2763 candidates carry exFAT's two-second timestamp granularity on
one side and 548 more were copied without -p.

Group on fits_header.header_fingerprint instead: written at scan time for
relinking, indexed, 100% populated, mtime-independent. Restricted to raw
sub-frames -- masters measure 0/30 precision because PixInsight propagates
the reference image's FITS keywords, so two filters can share a header.

The header join crosses two tables with no UNIQUE(file_id), so the count is
COUNT(DISTINCT f.id) and the id/path lists are de-duplicated in Rust: a
duplicated child row must never present a lone file as a group of two."
```

---

### Task 2: Cache tables carry the third key

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (extract the table DDL at 690-712 into a helper; add the guarded rebuild after `prune_orphaned_calibration_sets(conn)?;` ~line 1931)
- Modify: `crates/athenaeum-core/src/db/operations.rs` (`rebuild_duplicate_groups_cache` 2070, `get_cached_duplicates` 2186, `has_duplicate_cache` 2252)
- Modify: `crates/athenaeum-core/src/scanner/mod.rs:2399`
- Test: `crates/athenaeum-core/src/db/schema.rs` (new `#[cfg(test)] mod duplicate_cache_tests`)

**Interfaces:**
- Consumes: `DuplicateKey`, `hash_type()`, `hash_expr(alias)`, `joins(alias)`, `eligibility()`, `hash_is_usable(alias)` from Task 1.
- Produces: `rebuild_duplicate_groups_cache(conn, key: DuplicateKey) -> Result<usize>`, `get_cached_duplicates(conn, key: DuplicateKey) -> Result<Vec<DuplicateGroup>>`, `has_duplicate_cache(conn, key: DuplicateKey) -> Result<bool>`; private `create_duplicate_cache_tables(conn) -> rusqlite::Result<()>` in `schema.rs`. Task 3 calls the first three.

- [ ] **Step 1: Write the failing tests**

Add a new module at the end of `crates/athenaeum-core/src/db/schema.rs`:

```rust
#[cfg(test)]
mod duplicate_cache_tests {
    use super::*;
    use rusqlite::Connection;

    /// The shipped CHECK was `IN ('content', 'metadata')`. A catalog created
    /// before this change must accept 'header' after `init_db`, or every
    /// post-scan cache rebuild dies on a constraint violation and the
    /// Duplicates view silently recomputes on every open.
    #[test]
    fn init_db_widens_the_hash_type_check_on_an_old_catalog() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Reproduce the pre-fix shape exactly.
        conn.execute("DROP TABLE duplicate_group_files", []).unwrap();
        conn.execute("DROP TABLE duplicate_groups", []).unwrap();
        conn.execute(
            "CREATE TABLE duplicate_groups (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                hash TEXT NOT NULL,
                hash_type TEXT NOT NULL CHECK(hash_type IN ('content', 'metadata')),
                size INTEGER NOT NULL,
                file_count INTEGER NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                UNIQUE(hash, hash_type)
             )",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE duplicate_group_files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                group_id INTEGER NOT NULL,
                file_id INTEGER NOT NULL,
                FOREIGN KEY (group_id) REFERENCES duplicate_groups(id) ON DELETE CASCADE,
                FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE,
                UNIQUE(group_id, file_id)
             )",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO duplicate_groups (hash, hash_type, size, file_count)
                 VALUES ('h', 'header', 1, 2)",
                [],
            )
            .is_err(),
            "old shape must reject 'header' — otherwise this test proves nothing"
        );

        // Next app start.
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO duplicate_groups (hash, hash_type, size, file_count)
             VALUES ('h', 'header', 1, 2)",
            [],
        )
        .expect("init_db must widen the CHECK to accept 'header'");
        conn.execute(
            "INSERT INTO duplicate_groups (hash, hash_type, size, file_count)
             VALUES ('m', 'master', 1, 2)",
            [],
        )
        .expect("the same migration must admit 'master' — Task 7 writes it");
    }

    /// A current catalog is left alone — a second `init_db` must not drop a
    /// cache that was just rebuilt by a scan.
    #[test]
    fn a_current_catalog_keeps_its_cached_groups_across_init_db() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO duplicate_groups (hash, hash_type, size, file_count)
             VALUES ('h', 'header', 1, 2)",
            [],
        )
        .unwrap();

        init_db(&conn).unwrap();

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM duplicate_groups", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "an up-to-date cache must survive a restart");
    }

    /// The cache is built with the same key it is read back with, and the
    /// header key's cache never fans out on a duplicated child row (same
    /// hazard as `find_duplicate_groups` — see Task 1).
    #[test]
    fn cache_round_trips_under_the_header_key() {
        use crate::db::{
            get_cached_duplicates, has_duplicate_cache, rebuild_duplicate_groups_cache,
            DuplicateKey,
        };

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO scan_roots (path, find_duplicates) VALUES ('/vol', 1)",
            [],
        )
        .unwrap();
        for (id, path, mtime) in [
            (1i64, "/vol/a/f.fits", "2024-01-01T00:00:00+00:00"),
            (2, "/vol/b/f.fits", "2024-01-01T00:00:01+00:00"),
        ] {
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 'f.fits', 100, ?3, 'FITS')",
                rusqlite::params![id, path, mtime],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (?1, ?1, 'Flat', 0)",
                rusqlite::params![id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO fits_header (file_id, header, header_fingerprint)
                 VALUES (?1, 'HDR', ?2)",
                rusqlite::params![id, crate::fingerprint::compute_header_fingerprint("HDR")],
            )
            .unwrap();
        }
        // A duplicated child row for file 1 — the schema permits it.
        conn.execute(
            "INSERT INTO fits_header (file_id, header, header_fingerprint)
             VALUES (1, 'HDR', ?1)",
            rusqlite::params![crate::fingerprint::compute_header_fingerprint("HDR")],
        )
        .unwrap();

        let created = rebuild_duplicate_groups_cache(&conn, DuplicateKey::Header).unwrap();
        assert_eq!(created, 1);
        assert!(has_duplicate_cache(&conn, DuplicateKey::Header).unwrap());
        assert!(
            !has_duplicate_cache(&conn, DuplicateKey::Content).unwrap(),
            "the two keys must not share a cache"
        );

        let groups = get_cached_duplicates(&conn, DuplicateKey::Header).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].file_count, 2, "fan-out must not inflate the cache");
        assert_eq!(groups[0].file_ids.len(), 2);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p athenaeum-core --lib db::schema::duplicate_cache_tests`
Expected: `init_db_widens_…` FAILS with "CHECK constraint failed"; `cache_round_trips_…` FAILS to compile (`DuplicateKey` not accepted by the cache functions yet).

- [ ] **Step 3: Extract the table DDL into a helper**

In `crates/athenaeum-core/src/db/schema.rs`, replace the two `CREATE TABLE IF NOT EXISTS duplicate_groups` / `duplicate_group_files` statements at lines 690-712 with a single call `create_duplicate_cache_tables(conn)?;`, and add the helper beside `column_exists`:

```rust
/// The duplicate-groups cache, as `init_db` wants it. One definition shared by
/// the creation path and the header-key migration below, so the two can never
/// drift into disagreeing about the CHECK.
fn create_duplicate_cache_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS duplicate_groups (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            hash TEXT NOT NULL,
            hash_type TEXT NOT NULL CHECK(hash_type IN ('content', 'metadata', 'header', 'master')),
            size INTEGER NOT NULL,
            file_count INTEGER NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(hash, hash_type)
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS duplicate_group_files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            group_id INTEGER NOT NULL,
            file_id INTEGER NOT NULL,
            FOREIGN KEY (group_id) REFERENCES duplicate_groups(id) ON DELETE CASCADE,
            FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE,
            UNIQUE(group_id, file_id)
        )",
        [],
    )?;
    Ok(())
}

/// True when `duplicate_groups.hash_type` already accepts the current key
/// set. Probing for `'master'` alone is sufficient: 'header' and 'master' are
/// added by the same migration, so a DDL that knows 'master' knows both.
///
/// Reads the stored DDL rather than probing with a rolled-back INSERT: a
/// probe would open a savepoint in the middle of `init_db`, and the property
/// is cheap to read directly. A reformatted constraint would make this return
/// false and re-run the migration once, which drops and recreates a cache that
/// the next scan rebuilds anyway — a harmless false negative, and the only way
/// this check can be wrong.
fn duplicate_cache_accepts_current_hash_types(conn: &Connection) -> rusqlite::Result<bool> {
    let ddl: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'duplicate_groups'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(ddl.is_some_and(|sql| sql.contains("'master'")))
}
```

`optional()` needs `use rusqlite::OptionalExtension;` — add it to the file's imports if absent.

- [ ] **Step 4: Add the guarded rebuild**

In `init_db`, immediately after `prune_orphaned_calibration_sets(conn)?;` (~line 1931):

```rust
    // The duplicate cache learned a third `hash_type` ('header') when the
    // cheap key stopped being `files.metadata_hash`. SQLite cannot widen a
    // CHECK via ALTER, and the 12-step rebuild recipe is not worth running
    // here: both tables are DERIVED DATA — every row is recomputed by
    // `rebuild_duplicate_groups_cache` at the end of the next scan — so
    // dropping and recreating them is the correct migration and costs one
    // recompute.
    if !duplicate_cache_accepts_current_hash_types(conn)? {
        conn.execute("DROP TABLE IF EXISTS duplicate_group_files", [])?;
        conn.execute("DROP TABLE IF EXISTS duplicate_groups", [])?;
        create_duplicate_cache_tables(conn)?;
        tracing::info!("duplicate cache tables rebuilt for the header and master keys");
    }
```

The three `CREATE INDEX IF NOT EXISTS idx_dup_group_*` statements already sit in the index batch further down (lines 981-989) and run after this, so the recreated tables get their indexes on the same start. Leave them where they are.

- [ ] **Step 5: Thread the enum through the three cache functions**

`rebuild_duplicate_groups_cache`: change the signature to `(conn: &Connection, key: DuplicateKey)`, delete the `hash_type`/`hash_column` `if` blocks, and set `let hash_type = key.hash_type();`. Its main query becomes:

```rust
    let query = format!(
        "SELECT {hash}, f.size, COUNT(DISTINCT f.id) as count
         FROM files f
         {joins}
         WHERE {usable}
         AND NOT EXISTS (
             SELECT 1 FROM black_hole bh WHERE bh.file_id = f.id
         )
         {eligibility}
         AND ({roots})
         GROUP BY {hash}, f.size
         HAVING count > 1",
        hash = key.hash_expr("f"),
        joins = key.joins("f"),
        usable = key.hash_is_usable("f"),
        eligibility = key.eligibility(),
        roots = root_predicate,
    );
```

and its per-group member query — which is aliased `files`, not `f`, and which the existing code already re-binds the root predicate for — becomes:

```rust
    let files_query = format!(
        "SELECT DISTINCT files.id
         FROM files
         {joins}
         WHERE {hash} = ? AND files.size = ?
         AND NOT EXISTS (
             SELECT 1 FROM black_hole bh WHERE bh.file_id = files.id
         )
         {eligibility}
         AND ({roots})",
        hash = key.hash_expr("files"),
        joins = key.joins("files"),
        eligibility = key.eligibility(),
        roots = files_root_predicate,
    );
```

`SELECT DISTINCT` is the fan-out guard here — without it a duplicated `fits_header` row inserts the same `file_id` twice and the `UNIQUE(group_id, file_id)` on `duplicate_group_files` turns a benign schema quirk into a failed scan.

`get_cached_duplicates` and `has_duplicate_cache`: replace the `use_content_hash: bool` parameter with `key: DuplicateKey` and each local `if` with `let hash_type = key.hash_type();`. Nothing else in either body changes.

- [ ] **Step 6: Update the scanner call site**

`crates/athenaeum-core/src/scanner/mod.rs:2399`:

```rust
        // Rebuild the duplicate groups cache under the default (header) key.
        if let Err(e) = rebuild_duplicate_groups_cache(conn, DuplicateKey::Header) {
            result.errors.push(format!("Failed to rebuild duplicate cache: {}", e));
        }
```

Add `DuplicateKey` to the `use crate::db::{…}` list at the top of that file.

- [ ] **Step 7: Run the tests**

Run: `cargo test -p athenaeum-core --lib db::schema::duplicate_cache_tests db::operations`
Expected: PASS. Existing cache tests in `operations.rs` that passed `true` now pass `DuplicateKey::Content`.

- [ ] **Step 8: Commit**

```bash
git add crates/athenaeum-core/src/db/schema.rs crates/athenaeum-core/src/db/operations.rs crates/athenaeum-core/src/scanner/mod.rs
git commit -m "fix(duplicates): cache the header key alongside content

duplicate_groups.hash_type gains 'header' and 'master' (the latter written
by the master-duplicates key, added in this same cycle). SQLite cannot widen
a CHECK via ALTER, and both cache tables are derived data recomputed at the
end of every scan, so init_db drops and recreates them when the stored DDL
is the old one.
The per-group member query gains SELECT DISTINCT: fits_header has no
UNIQUE(file_id), and a duplicated row would otherwise trip
duplicate_group_files' UNIQUE(group_id, file_id) and fail the scan."
```

---

### Task 3: Both backends select the key

**Files:**
- Modify: `crates/athenaeum-core/src/api/files.rs:185-198`
- Modify: `crates/athenaeum-web/src/routes/duplicates.rs:72-97`
- Test: `crates/athenaeum-core/src/api/files.rs` (new `#[cfg(test)] mod duplicate_key_tests`)

**Interfaces:**
- Consumes: `DuplicateKey::from_setting`, `has_duplicate_cache`, `get_cached_duplicates`, `find_duplicate_groups` from Tasks 1-2; `ServiceContext::new_for_tests(PathBuf)`; `db::set_setting(conn, key, value)`; `settings::keys::DUPLICATES_USE_CONTENT_HASH`.
- Produces: nothing new; `get_duplicates(ctx) -> Result<Vec<DuplicateGroup>, ApiError>` keeps its signature.

- [ ] **Step 1: Write the failing test**

This exercises the handler end to end rather than asserting on the enum alone — the enum is already covered by Task 1, and what is untested here is that the handler picks the key from the setting.

Append to `crates/athenaeum-core/src/api/files.rs`:

```rust
#[cfg(test)]
mod duplicate_key_tests {
    use crate::services::ServiceContext;

    /// With the setting off, the handler must use the header key and find the
    /// pair whose mtimes drifted. With it on, it must use the content key and
    /// find nothing (no content index has been built), rather than silently
    /// falling back to the header key.
    #[test]
    fn get_duplicates_follows_the_use_content_hash_setting() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));

        {
            let db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
            let conn = db.conn();
            conn.execute(
                "INSERT INTO scan_roots (path, find_duplicates) VALUES ('/vol', 1)",
                [],
            )
            .unwrap();
            for (id, path, mtime) in [
                (1i64, "/vol/a/f.fits", "2024-10-05T04:21:46+00:00"),
                (2, "/vol/b/f.fits", "2024-10-05T04:21:44.307+00:00"),
            ] {
                conn.execute(
                    "INSERT INTO files (id, path, filename, size, modified_at, format)
                     VALUES (?1, ?2, 'f.fits', 100, ?3, 'FITS')",
                    rusqlite::params![id, path, mtime],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO frames (id, file_id, imagetyp, is_master)
                     VALUES (?1, ?1, 'Flat', 0)",
                    rusqlite::params![id],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO fits_header (file_id, header, header_fingerprint)
                     VALUES (?1, 'HDR', ?2)",
                    rusqlite::params![
                        id,
                        crate::fingerprint::compute_header_fingerprint("HDR")
                    ],
                )
                .unwrap();
            }
        }

        let groups = super::get_duplicates(&ctx).unwrap();
        assert_eq!(groups.len(), 1, "header key is the default, got {groups:#?}");
        assert_eq!(groups[0].file_count, 2);

        {
            let db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
            let conn = db.conn();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::DUPLICATES_USE_CONTENT_HASH,
                "true",
            )
            .unwrap();
        }

        let groups = super::get_duplicates(&ctx).unwrap();
        assert!(
            groups.is_empty(),
            "content key with no content index must find nothing, got {groups:#?}"
        );
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p athenaeum-core --lib api::files::duplicate_key_tests`
Expected: FAIL to compile — `get_duplicates` still calls the cache functions with a `bool`.

- [ ] **Step 3: Update the core handler**

`crates/athenaeum-core/src/api/files.rs`, replacing the body of `get_duplicates`:

```rust
pub fn get_duplicates(ctx: &ServiceContext) -> Result<Vec<DuplicateGroup>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let key = crate::db::DuplicateKey::from_setting(
        ctx.settings
            .get_duplicates_use_content_hash(&conn)
            .unwrap_or(false),
    );

    if crate::db::has_duplicate_cache(&conn, key).unwrap_or(false) {
        return Ok(crate::db::get_cached_duplicates(&conn, key)?);
    }
    Ok(crate::db::find_duplicate_groups(&conn, key)?)
}
```

- [ ] **Step 4: Update the Axum mirror**

`crates/athenaeum-web/src/routes/duplicates.rs`, lines 79-95 — same substitution, keeping the surrounding `.map_err(db_err)` / `Json(...)` shape exactly:

```rust
    let key = athenaeum_core::db::DuplicateKey::from_setting(
        state
            .settings
            .get_duplicates_use_content_hash(&conn)
            .unwrap_or(false),
    );

    if athenaeum_core::db::has_duplicate_cache(&conn, key).unwrap_or(false) {
        let groups = athenaeum_core::db::get_cached_duplicates(&conn, key).map_err(db_err)?;
        return Ok(Json(groups));
    }
    let groups = athenaeum_core::db::find_duplicate_groups(&conn, key).map_err(db_err)?;
```

- [ ] **Step 5: Run the gates**

Run: `cargo build --workspace && cargo test -p athenaeum-core`
Expected: build clean, all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/api/files.rs crates/athenaeum-web/src/routes/duplicates.rs
git commit -m "fix(duplicates): both backends select the duplicate key"
```

---

### Task 4: Folder similarity uses the same key

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations_blackhole.rs:414-435`
- Test: `crates/athenaeum-core/src/db/operations_blackhole.rs` (existing `#[cfg(test)] mod tests`, which already has `use super::*;` and `use crate::db::schema::init_db;`)

**Interfaces:**
- Consumes: nothing from Tasks 1-3 — this query is standalone and does not take a `DuplicateKey` (the function has no key parameter and gains none).
- Produces: `find_duplicate_folders(conn, similarity_threshold: f64) -> Result<Vec<FolderSimilarity>>` — signature unchanged. `FolderSimilarity`'s member count field is `shared_files`, not `shared_count`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `operations_blackhole.rs`:

```rust
/// Folder similarity grouped on `metadata_hash` too, so it was blind in
/// exactly the same way the Duplicates view was: two folders holding the same
/// twenty flats scored 0 % similar because one side came off an exFAT volume.
#[test]
fn folder_similarity_sees_copies_whose_mtime_drifted() {
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();

    for (id, path, mtime) in [
        (1i64, "/vol/a/f0.fits", "2024-10-05T04:21:46+00:00"),
        (2, "/vol/a/f1.fits", "2024-10-05T04:21:50+00:00"),
        (3, "/vol/b/f0.fits", "2024-10-05T04:21:44.307+00:00"),
        (4, "/vol/b/f1.fits", "2024-10-05T04:21:49.223+00:00"),
    ] {
        // Per-file metadata_hash, so the OLD key finds nothing and this test
        // is red before the change.
        let header = if path.ends_with("f0.fits") { "HDR-0" } else { "HDR-1" };
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, metadata_hash)
             VALUES (?1, ?2, 'f.fits', 100, ?3, 'FITS', ?4)",
            params![id, path, mtime, format!("meta-{id}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (?1, ?1, 'Flat', 0)",
            params![id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, ?2, ?3)",
            params![id, header, crate::fingerprint::compute_header_fingerprint(header)],
        )
        .unwrap();
    }

    let sims = find_duplicate_folders(&conn, 50.0).unwrap();
    assert_eq!(sims.len(), 1, "the two folders are full copies, got {sims:#?}");
    assert_eq!(sims[0].shared_files, 2);
    assert!((sims[0].similarity_percent - 100.0).abs() < 1e-6);
}

/// A master must not make two folders look alike — same exclusion as the
/// Duplicates view, for the same measured reason (spec §2.4).
#[test]
fn folder_similarity_ignores_masters() {
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();

    for (id, path) in [(1i64, "/vol/a/m.xisf"), (2, "/vol/b/m.xisf")] {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, metadata_hash)
             VALUES (?1, ?2, 'm.xisf', 100, '2024-01-01T00:00:00+00:00', 'XISF', ?3)",
            params![id, path, format!("meta-{id}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, is_master)
             VALUES (?1, ?1, 'MasterLight', 1)",
            params![id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, 'HDR', ?2)",
            params![id, crate::fingerprint::compute_header_fingerprint("HDR")],
        )
        .unwrap();
    }

    let sims = find_duplicate_folders(&conn, 50.0).unwrap();
    assert!(sims.is_empty(), "masters must not pair folders, got {sims:#?}");
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p athenaeum-core --lib db::operations_blackhole::tests::folder_similarity`
Expected: `folder_similarity_sees_copies_whose_mtime_drifted` FAILS with `left: 0, right: 1` (each file carries its own `metadata_hash`, so no hash is shared). `folder_similarity_ignores_masters` passes for the wrong reason today — it goes on guarding once the key changes.

- [ ] **Step 3: Swap the key**

In `find_duplicate_folders`, replace the file-loading statement (currently `SELECT id, path, metadata_hash, size FROM files WHERE metadata_hash IS NOT NULL AND NOT EXISTS (…)`) with:

```rust
    // Same identity the Duplicates view uses (`DuplicateKey::Header`): folder
    // similarity is the Duplicates question asked one directory at a time, so
    // keying it differently would make the two screens disagree about the same
    // pair of folders. Raw sub-frames only, for the reason in
    // `DuplicateKey::eligibility`. `DISTINCT` because neither `fits_header`
    // nor `frames` has a `UNIQUE(file_id)`, and a duplicated child row would
    // otherwise put one file into a folder's list twice and inflate its
    // similarity score.
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.id, f.path, fh.header_fingerprint, f.size
         FROM files f
         JOIN fits_header fh ON fh.file_id = f.id
         JOIN frames fr ON fr.file_id = f.id
         WHERE fh.header_fingerprint IS NOT NULL AND fh.header_fingerprint <> ''
         AND COALESCE(fr.is_master, 0) = 0
         AND fr.imagetyp IN ('Light', 'Flat', 'Dark', 'Bias', 'DarkFlat')
         AND NOT EXISTS (SELECT 1 FROM black_hole bh WHERE bh.file_id = f.id)"
    )?;
```

The rest of the function is unchanged — it already treats column 2 as an opaque hash string.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p athenaeum-core --lib db::operations_blackhole`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/db/operations_blackhole.rs
git commit -m "fix(duplicates): folder similarity groups on header identity too"
```

---

### Task 5: Say what the view actually does

**Files:**
- Modify: `src/pages/Settings.tsx` (the Duplicate Detection copy, ~lines 1160-1180)
- Modify: `src/components/duplicates/DuplicatesView.tsx` (one notice above the group list, ~line 629)

**Interfaces:**
- Consumes: the existing `useContentHash` state in `Settings.tsx`; the existing `duplicates` array in `DuplicatesView.tsx`.
- Produces: no new props or exports.

- [ ] **Step 1: Fix the Settings copy**

The current text tells the user the default groups "by size, date and filename" — a key that returns zero groups on a real 42 000-file catalog. Replace the two `<span className="block text-xs text-content-muted mt-1">` blocks under the checkbox label with:

```tsx
                  <span className="block text-xs text-content-muted mt-1">
                    Groups the Duplicates view by a hash of the file's bytes instead of by
                    its FITS/XISF header. Masters and processed files do not need this —
                    they are always matched by their full contents, because processing
                    tools copy a header verbatim onto a different image.
                  </span>
                  <span className="block text-xs text-content-muted mt-1">
                    Left off, raw sub-frames are grouped by their stored header, which every
                    scan already records — no extra reading, and copies still match after a
                    move between drives changed their timestamps. With this on, scans also
                    hash new and changed files as they go, which is slower on NAS or other
                    network storage; the content index below fills in the rest.
                  </span>
```

- [ ] **Step 2: Add the notice to the Duplicates view**

In `DuplicatesView.tsx`, directly above the rendered group list (beside the existing verify-summary alert around line 629):

```tsx
        {duplicates.length > 0 && (
          <p className="text-xs text-content-muted px-1 pb-2">
            Raw sub-frames are matched by their stored header. Masters and processed
            files are matched by their full contents instead — a processing tool can
            copy one image's header onto another, so a header does not identify them
            — and they appear here after the scan that hashes them.
          </p>
        )}
```

- [ ] **Step 3: Run the frontend gate**

Run: `npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add src/pages/Settings.tsx src/components/duplicates/DuplicatesView.tsx
git commit -m "docs(duplicates): describe the key the view actually uses"
```

---

### Task 6: Record the residue

**Files:**
- Modify: `docs/superpowers/open-items.md`

**Interfaces:**
- Consumes: nothing.
- Produces: nothing.

- [ ] **Step 1: Add the unverified-smoke block**

Under `## Unverified by hand`, newest first:

```markdown
### Duplicate detection keyed on header identity — 2026-08-27

The cheap duplicate key stopped being `files.metadata_hash`
(`size + mtime + filename`, where mtime is a property of the copy) and became
`(fits_header.header_fingerprint, files.size)` restricted to raw sub-frames.
Spec: `specs/2026-08-27-duplicate-detection-design.md`. On the owner's
production catalog the view returned 0 groups while holding 2 750
(5 552 files, 170.5 GiB) across 33 calibration sets.

- Open the Duplicates view on the production catalog: ~2 750 groups appear,
  including the twenty pairs in calibration set 628.
- No group mixes two filters. Spot-check any `C_2022_E3_ZTF_Light_*` or
  `IC_2087_Light_*` name — those files repeat across filter folders with the
  same size and must NOT be grouped.
- Master groups contain only byte-identical files: `Pane_2_Sii.xisf` /
  `Pane_2_Ha.xisf` (identical headers, different filters) never group, and
  neither do the `masterDark_BIN-1_…` near-copies that differ by ~12 bytes of
  XML header. NB: on this catalog every sampled master pair proved
  byte-DIFFERENT, so ZERO master groups is a legitimate — and expected —
  outcome; the smoke is that no wrong group appears, not that groups do.
- After the scan, `SELECT COUNT(*) FROM files WHERE strong_hash IS NOT NULL`
  is ~61 (the header-shortlisted masters), not 381 and not 41 893.
- Run a scan: the post-scan rebuild fills `duplicate_groups` with
  `hash_type = 'header'` and the second open of the view is instant.
- Turn on content grouping with an empty content index: the view goes empty
  rather than erroring, and the Settings text points at the index.
- `Find duplicate folders` scores the two copies of a flats folder as similar.
```

- [ ] **Step 2: Add the release-note line**

Under `## Release notes owed at the next tag`:

```markdown
- Duplicate detection now recognises copies whose timestamps changed in
  transit — moving a night between drives no longer hides its duplicates.
  Masters and processed files are compared by their full contents, so two
  different stacks that share a header are never mistaken for copies.
```

- [ ] **Step 3: Add the two standing decisions**

Under `## Standing decisions — do not re-flag these`:

```markdown
| **The XISF parser drops the `comment` attribute of `FITSKeyword`, and fixing it is not a duplicate-detection fix.** | `fits_parser/mod.rs`, stored `fits_header.header` blobs | PixInsight writes history as `value="" comment="ImageIntegration.rejectedHigh_32: …"`, so our blob holds 364 empty `HISTORY =` lines. Including `comment` separates only 4 of 30 master groups — the other 26 share every keyword and property and differ only in pixels, so masters stay excluded from the header key either way. Worth fixing for the metadata pane's per-field revert and light calibration's Bayer copy-through, which read that blob — but NOT in the same release as the duplicate-key change: a changed blob changes the fingerprint, so a re-scanned file stops matching its not-yet-re-scanned copy until both are scanned. |
| **The three-part sampling hash is not a better default key than the header.** | Any "just use `compute_xxhash`" proposal | It IS `files.content_hash`, so the proposal is the existing `Content` branch. Measured: identical answer to the header key on raw frames (40/40 vs 80/80 against full SHA-256) for 61.4 GiB of reads and ~19 min; and on masters it is wrong in the DELETING direction — three of thirty groups are `..._DBE_WCS.xisf` / `_f.xisf` pairs differing by 3-4 bytes at 0.5-0.9 MiB, past the first sample and nowhere near the middle or end. Spec §2.5. |
```

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/open-items.md
git commit -m "docs: record the duplicate-detection cycle in open-items"
```

---

### Task 7: Masters — shortlisted by header, decided by bytes

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (guarded `ALTER TABLE files ADD COLUMN strong_hash`, beside the `content_hash` migration at 1198-1209; index in the index batch ~line 865)
- Modify: `crates/athenaeum-core/src/db/operations.rs` (`DuplicateKey::Master` variant)
- Modify: `crates/athenaeum-core/src/duplicates/mod.rs` (`compute_full_xxhash`)
- Modify: `crates/athenaeum-core/src/duplicates/backfill.rs` (`fill_master_strong_hashes`, `MasterHashProgress`)
- Modify: `crates/athenaeum-core/src/ts_export.rs` (register `MasterHashProgress` beside `ContentIndexProgress` at line 154 — the `ts_contract` test enforces this)
- Modify: `crates/athenaeum-core/src/scanner/mod.rs` (phase-4 wiring ~2399; `strong_hash = NULL` in the in-place reparse UPDATE at 1325)
- Modify: `crates/athenaeum-core/src/api/files.rs` (`get_duplicates` unions Header + Master)
- Modify: `crates/athenaeum-web/src/routes/duplicates.rs` (same union)
- Test: `crates/athenaeum-core/src/db/operations.rs` and `crates/athenaeum-core/src/duplicates/backfill.rs`

**Interfaces:**
- Consumes: `DuplicateKey` and every accessor from Task 1; `Database`, `ProgressEmitter`, the `BackfillSummary` shape and the stale-row check idiom from `duplicates/backfill.rs::run_content_index`.
- Produces: `DuplicateKey::Master` (`hash_type() == "master"`); `duplicates::compute_full_xxhash(path: &Path) -> Result<String>`; `duplicates::backfill::fill_master_strong_hashes(conn: &Connection, emitter: &dyn ProgressEmitter, cancel: &AtomicBool) -> usize` returning the number of rows hashed. **`&Connection`, not `&Database`** — the phase-4 caller (`scan_directory_parallel`, verified signature: `root_path, root_id, conn, emitter, use_content_hash, cancel_flag, unique_camera`) has no `Database` in scope, and unlike `run_content_index` this pass runs inside a scan that already holds the connection, so the pool-slot argument for dropping it does not apply.

**Why this task exists.** The header key measures 0/30 precision on masters and no sampling scheme repairs that — detection tracks coverage linearly, and the real divergence is a single Float32 pixel, 4 bytes in 77 MiB (spec §2.4, §2.5). But the header key is an excellent *filter*: it takes 381 master files down to **61 candidates / 7.5 GiB**, which a full hash settles exactly in about a minute (spec §2.6). Header to narrow, bytes to decide.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `operations.rs` (it already has `seed_dup_file` / `seed_dup_root` from Task 1):

```rust
/// `strong_hash` for one file — what `fill_master_strong_hashes` would have
/// written after reading the bytes.
fn set_strong_hash(conn: &Connection, id: i64, hash: &str) {
    conn.execute(
        "UPDATE files SET strong_hash = ?2 WHERE id = ?1",
        params![id, hash],
    )
    .unwrap();
}

/// Two masters with the same header (PixInsight copies the reference image's
/// keywords) but different bytes must NOT group; two with the same bytes must.
/// This is the whole point of the Master key: the header shortlists, the bytes
/// decide.
#[test]
fn master_key_decides_by_bytes_not_by_header() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    seed_dup_root(&conn, "/vol", 1);

    // Sii / Ha: identical header, different pixels — must stay apart.
    seed_dup_file(&conn, 1, "/vol/a/Pane_2_Sii.xisf", "2024-01-01T00:00:00+00:00",
                  "HDR-SHARED", "MasterLight", 1);
    seed_dup_file(&conn, 2, "/vol/a/Pane_2_Ha.xisf", "2024-01-01T00:00:01+00:00",
                  "HDR-SHARED", "MasterLight", 1);
    set_strong_hash(&conn, 1, "bytes-sii");
    set_strong_hash(&conn, 2, "bytes-ha");

    // A genuine copy of one master in two places — must group.
    seed_dup_file(&conn, 3, "/vol/a/masterDark.xisf", "2024-01-01T00:00:02+00:00",
                  "HDR-DARK", "MasterDark", 1);
    seed_dup_file(&conn, 4, "/vol/b/masterDark.xisf", "2024-01-01T00:00:03+00:00",
                  "HDR-DARK", "MasterDark", 1);
    set_strong_hash(&conn, 3, "bytes-dark");
    set_strong_hash(&conn, 4, "bytes-dark");

    let groups = find_duplicate_groups(&conn, DuplicateKey::Master).unwrap();
    assert_eq!(groups.len(), 1, "only the byte-identical pair groups, got {groups:#?}");
    let mut ids = groups[0].file_ids.clone();
    ids.sort_unstable();
    assert_eq!(ids, vec![3, 4]);
}

/// A master whose bytes have not been hashed yet simply does not appear. A
/// missing hash is a miss, never a false positive — and never a NULL-groups-
/// with-NULL collapse.
#[test]
fn master_key_skips_files_with_no_strong_hash() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    seed_dup_root(&conn, "/vol", 1);

    seed_dup_file(&conn, 1, "/vol/a/m.xisf", "2024-01-01T00:00:00+00:00",
                  "HDR-M", "MasterDark", 1);
    seed_dup_file(&conn, 2, "/vol/b/m.xisf", "2024-01-01T00:00:01+00:00",
                  "HDR-M", "MasterDark", 1);
    // Neither has a strong_hash; one gets an empty string, which must be
    // rejected just like NULL.
    set_strong_hash(&conn, 2, "");

    assert!(find_duplicate_groups(&conn, DuplicateKey::Master).unwrap().is_empty());
}

/// The two keys partition the catalog: a raw sub-frame never reaches the
/// Master key, and a master never reaches the Header key. Nothing is decided
/// twice, and nothing falls between them.
#[test]
fn header_and_master_keys_partition_the_catalog() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    seed_dup_root(&conn, "/vol", 1);

    seed_dup_file(&conn, 1, "/vol/a/raw.fits", "2024-01-01T00:00:00+00:00",
                  "HDR-R", "Light", 0);
    seed_dup_file(&conn, 2, "/vol/b/raw.fits", "2024-01-01T00:00:01+00:00",
                  "HDR-R", "Light", 0);
    set_strong_hash(&conn, 1, "same");
    set_strong_hash(&conn, 2, "same");

    seed_dup_file(&conn, 3, "/vol/a/m.xisf", "2024-01-01T00:00:02+00:00",
                  "HDR-M", "MasterDark", 1);
    seed_dup_file(&conn, 4, "/vol/b/m.xisf", "2024-01-01T00:00:03+00:00",
                  "HDR-M", "MasterDark", 1);
    set_strong_hash(&conn, 3, "same-master");
    set_strong_hash(&conn, 4, "same-master");

    let header = find_duplicate_groups(&conn, DuplicateKey::Header).unwrap();
    assert_eq!(header.len(), 1);
    assert_eq!(header[0].file_ids.len(), 2);
    assert!(header[0].file_ids.contains(&1), "header key owns the raw frames");

    let master = find_duplicate_groups(&conn, DuplicateKey::Master).unwrap();
    assert_eq!(master.len(), 1);
    assert!(master[0].file_ids.contains(&3), "master key owns the masters");
    assert!(!master[0].file_ids.contains(&1), "a raw frame must not be decided twice");
}
```

And add to `crates/athenaeum-core/src/duplicates/backfill.rs`, **inside the
EXISTING `#[cfg(test)] mod tests` (line 260)** — it already defines
`CapturingEmitter`, a `ProgressEmitter` capturing `(event_name, payload)`
pairs into a `Mutex<Vec<_>>`, which this test reuses instead of inventing a
no-op emitter (none exists in `crate::events`):

```rust
    use std::io::Write as _;

    /// The shortlist is what keeps this affordable: only masters whose header
    /// already puts them in a group get hashed. A master with a unique header
    /// is nobody's duplicate, so reading 300 MiB to prove it would be waste.
    #[test]
    fn only_header_shortlisted_masters_are_hashed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();

        let write = |name: &str, body: &[u8]| -> String {
            let p = tmp.path().join(name);
            std::fs::File::create(&p).unwrap().write_all(body).unwrap();
            p.to_string_lossy().to_string()
        };
        let shared_a = write("a.xisf", b"AAAA");
        let shared_b = write("b.xisf", b"AAAA");
        let lonely = write("c.xisf", b"CCCC");

        for (id, path, header) in [
            (1i64, &shared_a, "HDR-SHARED"),
            (2, &shared_b, "HDR-SHARED"),
            (3, &lonely, "HDR-UNIQUE"),
        ] {
            let meta = std::fs::metadata(path).unwrap();
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 'x.xisf', ?3, ?4, 'XISF')",
                rusqlite::params![
                    id,
                    path,
                    meta.len() as i64,
                    chrono::DateTime::<chrono::Utc>::from(meta.modified().unwrap())
                        .to_rfc3339()
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp, is_master)
                 VALUES (?1, ?1, 'MasterLight', 1)",
                rusqlite::params![id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO fits_header (file_id, header, header_fingerprint)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    id,
                    header,
                    crate::fingerprint::compute_header_fingerprint(header)
                ],
            )
            .unwrap();
        }

        let emitter = CapturingEmitter(std::sync::Mutex::new(Vec::new()));
        let never = std::sync::atomic::AtomicBool::new(false);
        let n = fill_master_strong_hashes(&conn, &emitter, &never);
        assert_eq!(n, 2, "only the two shortlisted masters are read");

        let hashed: Vec<(i64, Option<String>)> = conn
            .prepare("SELECT id, strong_hash FROM files ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(hashed[0].1, hashed[1].1, "identical bytes, identical hash");
        assert!(hashed[0].1.is_some());
        assert!(hashed[2].1.is_none(), "the lonely master must not be read");

        // Idempotent: a second pass hashes nothing.
        assert_eq!(fill_master_strong_hashes(&conn, &emitter, &never), 0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p athenaeum-core --lib db::operations::tests::master_key duplicates::backfill::master_strong_hash_tests`
Expected: FAIL to compile — `no variant named Master`, `no column named strong_hash`, `cannot find function fill_master_strong_hashes`.

- [ ] **Step 3: Add the column**

In `crates/athenaeum-core/src/db/schema.rs`, directly after the `content_hash` migration block (1198-1209):

```rust
    // Full-file hash, used ONLY to decide master/processed duplicates.
    // Deliberately NOT `content_hash`: that column is the three-part sampling
    // hash and the transfer dedup handshake depends on that meaning, so
    // overloading it would silently change what a peer is told about a file.
    // NULL means "not hashed yet" — a miss, never a false positive.
    let has_strong_hash: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('files') WHERE name='strong_hash'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_strong_hash {
        conn.execute("ALTER TABLE files ADD COLUMN strong_hash TEXT", [])?;
    }
```

and add to the index batch beside `idx_files_metadata_hash` (~line 865):

```rust
        "CREATE INDEX IF NOT EXISTS idx_files_strong_hash ON files(strong_hash)",
```

- [ ] **Step 4: Add the `Master` variant**

In `DuplicateKey` (`operations.rs`), add the variant and extend all five `match self` arms:

```rust
    /// Full-file hash (`files.strong_hash`) over masters and processed files.
    ///
    /// Their headers are shared by construction — PixInsight propagates the
    /// integration reference's FITS keywords, so `Pane_2_Sii.xisf` states
    /// `FILTER = 'H'` — which makes the header useless as a verdict and
    /// excellent as a filter. [`crate::duplicates::backfill::fill_master_strong_hashes`]
    /// hashes only the masters the header already shortlists into a group: 61
    /// files / 7.5 GiB on the owner's catalog rather than 381 / 89.4 GiB.
    Master,
```

- `hash_type` → `Self::Master => "master"`
- `hash_expr` → `Self::Master => format!("{files_alias}.strong_hash")`
- `joins` → `Self::Master => format!("JOIN frames fr ON fr.file_id = {files_alias}.id")` (needed by `eligibility`)
- `eligibility` → the exact complement of `Header`'s allowlist, so the two keys partition the catalog and nothing is decided twice or falls between them:

```rust
            Self::Master => {
                "AND (COALESCE(fr.is_master, 0) = 1 \
                      OR fr.imagetyp NOT IN ('Light', 'Flat', 'Dark', 'Bias', 'DarkFlat'))"
            }
```

- `hash_is_usable` needs no arm — it is written in terms of `hash_expr`, and the existing `IS NOT NULL AND <> ''` is exactly right for `strong_hash`.

`from_setting` keeps returning only `Header`/`Content`: `Master` is not a user setting, it is the second half of the default view (Step 7).

- [ ] **Step 5: Add the full-file hash**

In `crates/athenaeum-core/src/duplicates/mod.rs`, beside `compute_xxhash`:

```rust
/// Hash a file's ENTIRE contents with XXH3_64.
///
/// The counterpart to [`compute_xxhash`], which samples three 512 KiB regions
/// and is documented as lossy. Sampling is not merely lossy here, it is
/// hopeless: measured over 20 000 trials, a sampling scheme's chance of
/// noticing a changed pixel equals the fraction of the file it reads, and the
/// real divergence between two PixInsight masters is ONE Float32 pixel — 4
/// bytes in 77 MiB. Spending more of the budget on more, smaller samples makes
/// it strictly worse. So masters are decided by reading everything.
///
/// Affordable only because the caller hashes a header-shortlisted subset (see
/// [`backfill::fill_master_strong_hashes`]): 7.5 GiB, not 2.62 TiB.
pub fn compute_full_xxhash(path: &Path) -> Result<String> {
    use std::io::Read;

    let mut file = File::open(path)?;
    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:016x}", hasher.digest()))
}
```

- [ ] **Step 6: Add the backfill pass**

In `crates/athenaeum-core/src/duplicates/backfill.rs`, mirroring `run_content_index`'s structure (snapshot the pending rows, drop the connection, hash, write back in chunks, stale-check each file's `size`/`modified_at` against the row before writing):

```rust
/// Fill `files.strong_hash` for every master or processed file the header key
/// shortlists into a group and that has no hash yet.
///
/// The shortlist is the whole economy of this pass. A master with a unique
/// header fingerprint is nobody's duplicate, so reading 300 MiB to prove it
/// would be pure waste; on the owner's catalog the shortlist is 61 files /
/// 7.5 GiB out of 381 / 89.4 GiB. Idempotent — only NULL-hash rows are
/// visited, so a re-run converges.
///
/// A file whose size or mtime no longer matches its row is skipped, not
/// hashed: the row is stale and the next scan will rewrite it (same stale-row
/// stance as [`run_content_index`]). A read failure is logged and skipped —
/// one unreadable master must not abandon the pass.
pub fn fill_master_strong_hashes(
    conn: &Connection,
    emitter: &dyn ProgressEmitter,
    cancel: &AtomicBool,
) -> usize {
    let pending: Vec<(i64, String, i64, String)> = {
        let sql = "SELECT f.id, f.path, f.size, f.modified_at
                   FROM files f
                   JOIN fits_header fh ON fh.file_id = f.id
                   JOIN frames fr ON fr.file_id = f.id
                   WHERE (f.strong_hash IS NULL OR f.strong_hash = '')
                     AND fh.header_fingerprint IS NOT NULL
                     AND fh.header_fingerprint <> ''
                     AND (COALESCE(fr.is_master, 0) = 1
                          OR fr.imagetyp NOT IN ('Light','Flat','Dark','Bias','DarkFlat'))
                     AND EXISTS (
                        SELECT 1 FROM files f2
                        JOIN fits_header fh2 ON fh2.file_id = f2.id
                        JOIN frames fr2 ON fr2.file_id = f2.id
                        WHERE f2.id <> f.id
                          AND f2.size = f.size
                          AND fh2.header_fingerprint = fh.header_fingerprint
                          AND (COALESCE(fr2.is_master, 0) = 1
                               OR fr2.imagetyp NOT IN ('Light','Flat','Dark','Bias','DarkFlat'))
                     )";
        match conn.prepare(sql).and_then(|mut stmt| {
            stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
        }) {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!(error = %e, "master strong-hash: failed to list pending rows");
                return 0;
            }
        }
    };

    if pending.is_empty() {
        tracing::debug!(pending = 0, "master strong-hash: nothing to do");
        return 0;
    }
    tracing::info!(pending = pending.len(), "master strong-hash: pass starting");

    let total = pending.len();
    let mut written = 0usize;
    for (idx, (id, path, size, modified_at)) in pending.into_iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            tracing::info!(written, "master strong-hash: cancelled");
            break;
        }

        let p = std::path::Path::new(&path);
        // Stale-row check: hash only what the catalog still describes.
        match std::fs::metadata(p) {
            Ok(m) => {
                let on_disk_mtime = chrono::DateTime::<chrono::Utc>::from(
                    m.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                )
                .to_rfc3339();
                if m.len() as i64 != size || on_disk_mtime != modified_at {
                    tracing::debug!(file_id = id, path = %path,
                        "master strong-hash: row is stale, skipping");
                    continue;
                }
            }
            Err(e) => {
                tracing::warn!(file_id = id, path = %path, error = %e,
                    "master strong-hash: stat failed, skipping");
                continue;
            }
        }

        let hash = match crate::duplicates::compute_full_xxhash(p) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(file_id = id, path = %path, error = %e,
                    "master strong-hash: read failed, skipping");
                continue;
            }
        };

        if let Err(e) = conn.execute(
            "UPDATE files SET strong_hash = ?2 WHERE id = ?1",
            rusqlite::params![id, hash],
        ) {
            tracing::error!(file_id = id, error = %e, "master strong-hash: write failed");
            continue;
        }
        written += 1;

        emit_event(
            emitter,
            "master-hash-progress",
            &MasterHashProgress { done: idx + 1, total, path: path.clone() },
        );
    }

    tracing::info!(written, total, "master strong-hash: pass finished");
    written
}
```

Add the payload struct beside `ContentIndexProgress`, with the same derives (the emit helper is the file's existing `emit_event`):

```rust
/// Per-file progress of the master strong-hash pass. UI data for the scan
/// progress surface, not a log line — the pass logs its own lines separately.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct MasterHashProgress {
    pub done: usize,
    pub total: usize,
    pub path: String,
}
```

and register it in `crates/athenaeum-core/src/ts_export.rs` directly beside `crate::duplicates::backfill::ContentIndexProgress` (line 154) — the `ts_contract` test fails otherwise. The function needs `use rusqlite::Connection;` and `use std::sync::atomic::{AtomicBool, Ordering};` — check the file's existing imports and add only what is missing (`Arc` may become unused in this file's imports if nothing else uses it; leave `run_content_index`'s own signature alone).

- [ ] **Step 7: Wire it in and invalidate on reparse**

`crates/athenaeum-core/src/scanner/mod.rs`, phase 4 (~2399), BEFORE the cache rebuilds — the Master cache can only be built once the hashes exist:

The enclosing function is `scan_directory_parallel<E: ProgressEmitter>(root_path, root_id, conn, emitter, use_content_hash, cancel_flag, unique_camera)` — `conn: &Connection`, `emitter: &E`, `cancel_flag: Arc<AtomicBool>` are all in scope, and there is no `Database` handle (which is why `fill_master_strong_hashes` takes `&Connection`). `&E` coerces to `&dyn ProgressEmitter` at the call:

```rust
        // Masters are shortlisted by header and decided by bytes: hash the
        // shortlist before the caches are rebuilt. Bounded by the shortlist,
        // not by the master population (61 files / 7.5 GiB vs 381 / 89.4 GiB
        // on the owner's catalog), and a scan has just read the whole library
        // anyway.
        crate::duplicates::backfill::fill_master_strong_hashes(conn, emitter, &cancel_flag);

        if let Err(e) = rebuild_duplicate_groups_cache(conn, DuplicateKey::Header) {
            result.errors.push(format!("Failed to rebuild duplicate cache: {}", e));
        }
        if let Err(e) = rebuild_duplicate_groups_cache(conn, DuplicateKey::Master) {
            result.errors.push(format!("Failed to rebuild master duplicate cache: {}", e));
        }
```

In the in-place reparse UPDATE (`scanner/mod.rs:1325`), add the invalidation:

```rust
    conn.execute(
        "UPDATE files
         SET size = ?1, modified_at = ?2, format = ?3,
             metadata_hash = ?4, content_hash = ?5,
             -- The bytes just changed, so any full-file hash is now a lie.
             -- NULL means "not hashed yet", which drops the row out of the
             -- Master key until the next pass re-reads it: a miss, never a
             -- stale group offered for deletion.
             strong_hash = NULL
         WHERE id = ?6",
```

- [ ] **Step 8: Show master groups in the view**

`crates/athenaeum-core/src/api/files.rs` — in the default (non-content) mode the view is the union of the two keys, because together they partition the catalog:

```rust
pub fn get_duplicates(ctx: &ServiceContext) -> Result<Vec<DuplicateGroup>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    // Content mode is a single explicit key over every file. Otherwise the
    // view is Header (raw sub-frames, decided by their stored header) plus
    // Master (everything else, decided by a full-file hash) — the two
    // eligibility clauses are exact complements, so no file is decided twice.
    let keys: &[crate::db::DuplicateKey] = if ctx
        .settings
        .get_duplicates_use_content_hash(&conn)
        .unwrap_or(false)
    {
        &[crate::db::DuplicateKey::Content]
    } else {
        &[crate::db::DuplicateKey::Header, crate::db::DuplicateKey::Master]
    };

    let mut all = Vec::new();
    for &key in keys {
        let groups = if crate::db::has_duplicate_cache(&conn, key).unwrap_or(false) {
            crate::db::get_cached_duplicates(&conn, key)?
        } else {
            crate::db::find_duplicate_groups(&conn, key)?
        };
        all.extend(groups);
    }
    all.sort_by(|a, b| b.file_count.cmp(&a.file_count).then(b.size.cmp(&a.size)));
    Ok(all)
}
```

Mirror the same block in `crates/athenaeum-web/src/routes/duplicates.rs`, keeping its `.map_err(db_err)` / `Json(...)` shape.

Task 3's test `get_duplicates_follows_the_use_content_hash_setting` still passes unchanged: its fixture holds only raw frames, so the Master key contributes nothing.

- [ ] **Step 9: Run the gates**

Run: `cargo build --workspace && cargo test -p athenaeum-core && npx tsc --noEmit`
Expected: all green.

- [ ] **Step 10: Commit**

```bash
git add crates/athenaeum-core/src/db/schema.rs crates/athenaeum-core/src/db/operations.rs \
        crates/athenaeum-core/src/duplicates/mod.rs crates/athenaeum-core/src/duplicates/backfill.rs \
        crates/athenaeum-core/src/scanner/mod.rs crates/athenaeum-core/src/api/files.rs \
        crates/athenaeum-web/src/routes/duplicates.rs
git commit -m "feat(duplicates): decide master duplicates by full-file hash

A master's header is shared by construction -- PixInsight propagates the
integration reference's FITS keywords, so Pane_2_Sii.xisf states FILTER='H'
-- which measures 0/30 precision, and no sampling scheme repairs it:
detection tracks coverage linearly and the real divergence is one Float32
pixel, 4 bytes in 77 MiB.

So the header shortlists and the bytes decide. New files.strong_hash column
(never content_hash -- that is the sampling hash the transfer dedup
handshake depends on), filled at scan time for the header-shortlisted subset
only: 61 files / 7.5 GiB instead of 381 / 89.4 GiB. The Header and Master
eligibility clauses are exact complements, so no file is decided twice."
```

---

## Self-Review

**Spec coverage.** D1 → Task 1. D2 (`metadata_hash` stays a column) → no task by construction; nothing removes it, and `MissingMetadataRow`'s `has_duplicate` flag reads `duplicate_group_files`, which Task 2 keeps populated. D3 (masters shortlisted by header, decided by bytes) → Task 7. D3a (`strong_hash`, never `content_hash`) → Task 7 Step 3 and its commit message. D3b (hash only the shortlist, at scan time) → Task 7 Steps 6-7. D4 → Task 1 Step 3. D5 → Task 2 (CHECK carries all four values including Task 7's `'master'` from the start, so the migration runs once — verified against the shipped DDL, which Task 2's widen-test reproduces byte-for-byte and asserts on both new values). D6 (verify stays advisory) → deliberately no task; `effectiveMoveDisabled = moveDisabled` is left as is. D7 → Task 4. Spec §2.5 → Task 6's standing decision + Task 7's `compute_full_xxhash` doc. Spec §2.6 → Task 7's shortlist SQL. Spec §5 → Tasks 1, 4, 5, 7. Spec §6 → Task 6 Step 3.

**Placeholder scan.** No TBD, no "add error handling", no "check what exists and use that" — the three placeholders the first draft carried are resolved against the verified code: the phase-4 scope is `conn`/`emitter`/`cancel_flag` with no `Database` (hence `fill_master_strong_hashes(&Connection, …)`), the progress helper is `emit_event` with a registered `ts_rs` payload struct, and the backfill test reuses the existing `CapturingEmitter` in the existing `mod tests` because no no-op emitter exists in `crate::events`.

**Type consistency.** `DuplicateKey` is defined in Task 1 with two variants and every `match self` exhaustive (no `_ =>`), so Task 7's `Master` variant turns each site into a compile error rather than a silent default; Task 7 names each arm it adds. `hash_type()` returns `&'static str` (`"header"` / `"content"` / `"master"` — all three admitted by Task 2's CHECK). `hash_expr`/`joins`/`hash_is_usable` take `files_alias: &str` and return `String` from their first definition; `eligibility()` stays `&'static str` and aliases only `fr`. Header and Master `eligibility` clauses are exact logical complements (`A ∧ B` vs `¬A ∨ ¬B`), pinned by `header_and_master_keys_partition_the_catalog`. `fill_master_strong_hashes(conn: &Connection, emitter: &dyn ProgressEmitter, cancel: &AtomicBool) -> usize` is identical in the interface block, the code, the scanner call (`(conn, emitter, &cancel_flag)` — `&E → &dyn` coercion) and both tests. `FolderSimilarity`'s field is `shared_files` (verified `models.rs:256`). `seed_dup_file`/`seed_dup_root`/`set_strong_hash` live in `operations.rs`'s test module and are used only there.

**Fan-out audit.** `frames`/`fits_header` have no `UNIQUE(file_id)`, so every query that joins them is guarded: `find_duplicate_groups` — `COUNT(DISTINCT f.id)` + Rust de-dup of both `GROUP_CONCAT` lists; `rebuild_duplicate_groups_cache` — `COUNT(DISTINCT f.id)` + `SELECT DISTINCT files.id`; `find_duplicate_folders` — `SELECT DISTINCT`; Task 7's shortlist SQL — feeds a per-id `UPDATE`, where a fanned duplicate id costs one redundant hash, never a wrong row. Regression tests inserting a second `fits_header` row: Task 1 Step 1, Task 2 Step 1.

**Order of execution.** Tasks land 1 → 7; each compiles and passes gates at its own commit. Task 3's handler body is intentionally superseded by Task 7 Step 8 (union of Header + Master); Task 3's test keeps passing because its fixture holds only raw frames. Task 7's cache write requires Task 2's CHECK to already contain `'master'` — it does, by Fix A, so no schema edit happens outside Task 2.
