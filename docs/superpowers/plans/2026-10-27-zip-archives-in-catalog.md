# ZIP Archives in the Catalog — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. In this repo the `lean-execution` skill overrides subagent-driven-development where they conflict. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Catalog FITS/XISF frames inside `.zip` files by reading only zip metadata, extract their bytes on demand (a size-limited cache or a permanent unpack), and put our own Move and ZIP archive on the same "file in a zip" model.

**Architecture:**
- A new core module `zip_store` owns all zip I/O: reading the directory and headers, extracting entries, the cache and `materialize`. No other module opens a zip.
- Consumers keep working with catalog path strings. At the point they read bytes, they swap the catalog path for a local path through `zip_store::materialize_*`. Because `files.path` is UNIQUE, the catalog path identifies the row.
- The scanner gains a zip pass. Black Hole, the file browser, rename/move and relink learn the zip as a whole-file item.

**Tech Stack:** Rust (`zip` 8.6 with deflate, `xxhash-rust`, rusqlite), Tauri 2 + Axum mirrors, React/TS with design tokens.

**Spec:** `docs/superpowers/specs/2026-10-27-zip-archives-in-catalog-design.md` (approved 2026-10-27). Phase-0 numbers: `docs/superpowers/research/2026-10-27-zip-probe-measurement.md`.

**Branch:** `feature/zip-archives` off `main`. Merge to local `main` when the plan is done; push only on the owner's word.

## Global Constraints

- **Two backends in sync.** Every new or changed Tauri command gets its Axum route in the same task. Logic lives in `athenaeum-core`; both hosts are thin.
- **Boundary rules.** No `@tauri-apps/*` outside `src/api/`. Serde types crossing to TS use `#[serde(rename_all = "camelCase")]` and are registered in `ts_export.rs`. Regenerate with `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`.
- **Logging and errors.** Every command boundary wears `#[tracing::instrument(skip_all, err)]` (Axum: `err(Debug)`). Use `tracing` only, with no `println!`/`eprintln!`. New field names are `zip_path`, `zip_entry`, `bytes_read`, `cache_hit` (logging spec update in Task 16). Never swallow an error: log, then return it.
- **Zips are read-only.** No code writes into a zip. Only an explicit Send to Void of a zip deletes one.
- **Missing never deletes.** Rows whose zip is missing become missing; no code path deletes them.
- **Read sizes.**
  - The header pass reads through `BufReader::with_capacity(4 * 1024, …)`. Extraction uses `1024 * 1024`. Phase 0 measured 0.06% of a zip at 4 KiB and 9% at 1 MiB.
  - Frame extensions: `fits`, `fit`, `fts`, `xisf` (case-insensitive). Skip entries under `__MACOSX/`, entries whose file name starts with `._`, encrypted entries, entries that are themselves `.zip`, and names that are absolute or contain a `..` component.
- **Cache.**
  - Default limit 32 GB (setting `archive.zip_cache_limit_gb`).
  - Location: `<app data dir>/zip-cache/`, where the app data dir is the DB parent (as `api/hips.rs` does). `$ATHENAEUM_ZIP_CACHE` overrides it.
  - Layout: `<cache>/<key>/<entry file name>`.
- **Virtual path.** A foreign zip-resident row has `files.path = <zip path> + MAIN_SEPARATOR + <entry path with '/' turned into MAIN_SEPARATOR>`.
- **Frontend.**
  - Design tokens only. Shared primitives from `src/components/ui/` (`Chip`, `Button`, `DialogShell`); a new modal uses `DialogShell`.
  - `notify()` on failure only. Settings never get a Save button.
  - Listeners use the StrictMode-safe pattern from `CLAUDE.md`.
- **Repo wording.** UI strings, docs and commits are in English. Never name another code base. Tables use `| ---- |` separators.
- **Commits.** Author `eg013ra1n`, ending with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. One compile at a time on the dev Mac; never two cargo builds against the same `target/`.
- **Gate before merge.** `cargo test -p athenaeum-core` (all targets), `cargo test --workspace`, `npm run build`, `npx vitest run`.

## Plan decisions (beyond the spec)

1. **`materialize` takes catalog path strings, not file ids.**
   - The spec's §7.4 sketch takes file ids. Several consumers hold only a path: `WbppPlacement`, `ResolvedMaster`, the desktop preview (the frontend sends `frame.file.path`), and project `landed_path`. `files.path` is UNIQUE, so the path is the key. `estimate_zip_extraction` still takes file ids, because the dialogs have them.
   - A path with no `files` row, or with a disk row, passes through untouched. So does a path that is not in the catalog (an uncataloged master preview).
2. **CRC32 is not added to `models::File`.**
   - Adding a field to `File` breaks about 15 struct literals and the 51-column `FILE_FRAME_SELECT` projection. `zip_store` reads `files.archive_entry_crc32` by its own SQL.
3. **Black Hole for zips is a sibling table `black_hole_zips`**, not a nullable `black_hole.file_id`. SQLite cannot drop NOT NULL without rebuilding the table, and `black_hole` has a FK-cascade and a unique index.
   - This adds four commands beyond the spec's five: `move_zip_to_black_hole`, `get_black_hole_zips`, `restore_zip_from_black_hole`, `send_zip_to_void`. The count goes up by nine.
4. **A zip entry may adopt a zip-resident row whose own zip is gone.** This covers a zip moved or renamed outside the app, with the same offline-root guard on the old zip path. Disk files never adopt zip-resident rows (the §7.2 hijack fix).
5. **Our zipped rows unpacked to a folder use Unarchive's alternate target.** The permanent unpack (`unpack_zip_frames`) handles foreign rows. Both place each entry through one shared `zip_store::place_entry`.

## Review Focus

1. **The share disappears mid-extraction** (NAS unmounted during a stack). The frame fails with "inside <zip>, which is not reachable". No partial file is left in the cache, and the operation applies its existing per-frame failure policy. Pinned in Task 6 (`extraction_of_a_vanished_zip_fails_the_frame_and_leaves_no_partial_file`).
2. **Hostile or odd entry names** (`../x.fits`, `/abs.fits`, CP437 names, the same file name in two folders). Nothing is written outside the cache key dir or the unpack target, and a skipped entry logs `zip_entry`. Pinned in Task 2 (`unsafe_entry_names_are_skipped`) and Task 10 (`unpack_never_writes_outside_the_target`).
3. **macOS Archive Utility zips** carry `__MACOSX/._Light_001.fit` AppleDouble entries with a frame extension. They must not become frames. Pinned in Task 4 (`appledouble_entries_are_not_cataloged`).
4. **A user re-zips a night with more frames** (the zip's stat changes). Existing entries keep their rows by entry path, new entries are added, and entries that are gone go missing. Pinned in Task 4 (`changed_zip_keeps_known_rows_adds_new_and_leaves_vanished_missing`).
5. **Two zips hold the same entry path** (`Lights/L_001.fits` in both). The cache keys differ and the cached files never collide. Pinned in Task 6 (`same_entry_path_in_two_zips_caches_separately`).

---

### Task 1: Schema for zips

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (in `init_db`, after the `archive_path_in_zip` migration at ~:2264; FK indexes in the batch at ~:3240)
- Test: same file, new `#[cfg(test)] mod zip_schema_tests`

**Interfaces:**
- Produces:
  - Tables:
    - `zip_archives(id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, size INTEGER NOT NULL, modified_at TEXT NOT NULL, frame_entries INTEGER NOT NULL DEFAULT 0, scanned_at TEXT NOT NULL, created_by_operation INTEGER NULL REFERENCES archive_operations(id))`
    - `zip_released_entries(id INTEGER PRIMARY KEY, zip_id INTEGER NOT NULL REFERENCES zip_archives(id) ON DELETE CASCADE, entry_path TEXT NOT NULL, UNIQUE(zip_id, entry_path))`
    - `black_hole_zips(id INTEGER PRIMARY KEY, zip_id INTEGER NOT NULL UNIQUE REFERENCES zip_archives(id) ON DELETE CASCADE, from_where TEXT NOT NULL, moved_at TEXT NOT NULL, original_path TEXT NOT NULL)`
  - Column `files.archive_entry_crc32 INTEGER NULL`.
  - Index `idx_files_archive_zip_path ON files(archive_zip_path)`.
  - Backfill: one `zip_archives` row per distinct `archive_operation_files.target_zip_path` that has a `files` row marked archived. The stat comes from disk if the zip exists, else `size = 0, modified_at = ''`, so the next scan re-reads it. `created_by_operation` is the op id.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn init_db_creates_zip_tables_and_is_idempotent() {
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    init_db(&conn).unwrap();
    for t in ["zip_archives", "zip_released_entries", "black_hole_zips"] {
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1", [t], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "{t}");
    }
    assert!(column_exists(&conn, "files", "archive_entry_crc32").unwrap());
}

#[test]
fn backfill_creates_one_zip_archive_row_per_existing_archive_zip() {
    // two archive_operation_files rows into one zip + one into another, files marked archived
    // → init_db again → 2 zip_archives rows, created_by_operation set, size 0 when the zip is absent
}
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p athenaeum-core zip_schema_tests`. Expected: FAIL (no such table).
- [ ] **Step 3: Implement the tables, the column and the backfill** in `init_db`. Use the `column_exists` helper (:626) and `CREATE TABLE IF NOT EXISTS`. Run the backfill only when `zip_archives` is empty.
- [ ] **Step 4: Run** `cargo test -p athenaeum-core zip_schema_tests every_foreign_key_child_column_is_indexed`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat(zip): schema — zip_archives, released entries, black_hole_zips, entry CRC32`.

### Task 2: `zip_store` reader — directory, headers, extraction

**Files:**
- Create: `crates/athenaeum-core/src/zip_store/mod.rs`, `zip_store/reader.rs`
- Modify: `crates/athenaeum-core/src/lib.rs` (add `pub mod zip_store;`)
- Test: `zip_store/reader.rs` `mod reader_tests`. Zips come from `archive::zip_writer::build_zip(…, ArchiveCompression::{Store, Deflate})`; FITS from `archive::restore::tests::write_minimal_fits`.

**Interfaces:**
- Produces (in `zip_store::reader`):

```rust
pub struct ZipEntryInfo { pub index: usize, pub path: String, pub size: u64, pub compressed_size: u64,
                          pub crc32: u32, pub stored: bool, pub modified: Option<chrono::NaiveDateTime>, pub is_frame: bool }
pub struct ZipListing { pub entries: Vec<ZipEntryInfo>, pub skipped: Vec<(String, &'static str)>, pub bytes_read: u64 }
pub fn list_entries(zip: &Path) -> Result<ZipListing>;                       // 4 KiB buffer
pub fn read_entry_header(zip: &Path, entry: &str) -> Result<(Vec<u8>, u64)>; // header bytes, bytes_read
pub fn read_headers(zip: &Path, entries: &[&str]) -> Result<(Vec<(String, Result<Vec<u8>>)>, u64)>; // one open, 4 KiB
pub struct Extracted { pub bytes: u64, pub strong_hash: String }
pub fn extract_entry(zip: &Path, entry: &str, expected_crc: Option<u32>, dest: &Path,
                     cancel: &AtomicBool, on_bytes: &mut dyn FnMut(u64)) -> Result<Extracted>;   // 1 MiB buffer
pub fn is_safe_entry_name(name: &str) -> bool;
```

- Notes:
  - `read_headers` reads until the FITS `END` card (80-byte cards in 2880-byte blocks) or the XISF length (`XISF0100` + u32 LE + 4 reserved bytes), as `examples/zip_probe.rs` does. It caps the read at 200 blocks.
  - `extract_entry` writes to `dest.with_extension("partial")` and renames to `dest` on success. It deletes the partial file on any error or on cancel.
  - The zip reader checks the CRC itself. A mismatch with `expected_crc` from the catalog is an error too: "entry changed since it was cataloged".
  - `strong_hash` is streaming xxh3 over the full bytes, in the `package::xxh3_full_file` format.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn lists_frames_and_skips_non_frames() { /* zip: L_001.fits, notes.txt, sub/L_002.FIT → is_frame on 2, notes.txt is_frame=false */ }
#[test] fn unsafe_entry_names_are_skipped() { /* "../x.fits", "/abs.fits", "a/../../b.fit", "__MACOSX/._L.fit", "._L.fit", "inner.zip" → in skipped, never in entries as frames */ }
#[test] fn header_read_stops_at_end_card_store_and_deflate() {
    // 3-block header + 8 MiB data, both methods: header bytes == the file's first 3*2880 bytes;
    // bytes_read <= 64 * 1024
}
#[test] fn xisf_header_read_uses_the_declared_length() { /* header len 5000 → returned len 16 + 5000 */ }
#[test] fn extract_matches_source_and_hashes_like_xxh3_full_file() {
    // assert_eq!(fs::read(dest), fs::read(src)); assert_eq!(x.strong_hash, package::xxh3_full_file(&src)?)
}
#[test] fn extract_rejects_a_wrong_expected_crc_and_leaves_no_file() { /* Some(crc ^ 1) → Err containing "changed since it was cataloged"; !dest.exists() && !partial.exists() */ }
#[test] fn cancelled_extract_leaves_no_partial_file() {}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core zip_store::reader`. Expected: FAIL (module missing).
- [ ] **Step 3: Implement the functions above** in `zip_store/reader.rs`.
- [ ] **Step 4: Run the same command.** Expected: PASS.
- [ ] **Step 5: Commit** with `feat(zip): zip_store reader — directory, header-only reads, verified extraction`.

### Task 3: Header parsing from bytes

**Files:**
- Modify: `crates/athenaeum-core/src/fits_parser/fits_header_reader.rs` (make the `parse_from_reader` path reachable from bytes), `fits_parser/mod.rs` (`parse_fits_with_header` at :141, `read_xisf_header` at :18, `extract_xisf_header` at :150, `parse_xisf` at :580)
- Test: `fits_parser/mod.rs` new `mod from_bytes_tests`

**Interfaces:**
- Produces:

```rust
pub fn parse_fits_header_bytes(bytes: &[u8], file_id: i64, display_path: &Path) -> Result<(Frame, String)>;
pub fn parse_xisf_header_bytes(bytes: &[u8], file_id: i64, display_path: &Path) -> Result<(Frame, String)>;
```

- `display_path` is the virtual path. It drives the filename "master" heuristic in `finalize_frame` (:338) and the XISF fallback text (:228-230) exactly as a disk path does.
- The disk functions become thin wrappers: read the header bytes from the file, then call these. One header code path.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn bytes_and_disk_parse_give_identical_frame_header_and_fingerprint() {
    // write_minimal_fits(p); let (f1, h1) = parse_fits_with_header(&p, 7)?;
    // let (f2, h2) = parse_fits_header_bytes(&fs::read(&p)?[..2880*n], 7, &p)?;
    // assert_eq!(h1, h2); assert_eq!(compute_header_fingerprint(&h1), compute_header_fingerprint(&h2));
    // assert_eq!(serde_json::to_value(f1)?, serde_json::to_value(f2)?);
    // same for an XISF fixture (analysis/input.rs:288 writes one with write_xisf_f32)
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core from_bytes_tests`. Expected: FAIL.
- [ ] **Step 3: Implement both functions and rewire the disk wrappers.**
- [ ] **Step 4: Run** `cargo test -p athenaeum-core fits_parser scanner`. Expected: PASS (the existing parser and scanner tests stay green).
- [ ] **Step 5: Commit** with `refactor(fits): header parse from bytes; disk parse wraps it`.

### Task 4: Scanner zip pass

**Files:**
- Create: `crates/athenaeum-core/src/zip_store/scan.rs`
- Modify: `crates/athenaeum-core/src/scanner/mod.rs`:
  - pruning: `is_app_metadata_dir` (:148) also prunes `.athenaeum_staging` and `.athenaeum_restore_temp`;
  - both walkers (`scan_directory` :153, `scan_directory_parallel` :1523) collect `.zip` paths and run the zip pass after the frame files;
  - move detection (:506 and :1946) uses the shared lookup.
- Modify: `src/` scan progress label map (find where the `"caching"` phase label lives) to add `"zips"` → "Reading zip archives".
- Test: `zip_store/scan.rs` `mod scan_tests`; `scanner/mod.rs` `mod moved_file_guard_tests` (add one).

**Interfaces:**
- Consumes: Task 2 `list_entries`, `read_headers`; Task 3 `parse_*_header_bytes`; `db::insert_file`, `insert_frame`, `insert_fits_header`, `SavepointGuard`; `calibrated_light_identity`; `path_under_unavailable_scan_root` (:82).
- Produces:

```rust
pub struct ZipScanOutcome { pub zip_id: i64, pub skipped_unchanged: bool, pub inserted: Vec<(i64, ImageType)>,
                            pub adopted: usize, pub bytes_read: u64 }
pub fn scan_zip(conn: &Connection, zip: &Path, root_id: i64, unique_camera: bool) -> Result<ZipScanOutcome>;
pub fn virtual_path(zip: &Path, entry: &str) -> String;
/// Shared move lookup: the row a new file at `new_path` with this fingerprint should take over, if any.
pub(crate) fn find_moved_row(conn: &Connection, fingerprint: &str, new_path: &str, for_zip_entry: bool) -> Result<Option<(i64, String)>>;
```

- `scan_zip` order:
  1. Stat; equal to the `zip_archives` row → `skipped_unchanged`, with no open.
  2. `list_entries`.
  3. Entries already owned by a row (`archive_zip_path = zip AND archive_path_in_zip = entry`) are known. A known entry whose CRC changed is re-parsed and updated in place, like a modified disk file. Entries in `zip_released_entries` are skipped.
  4. The rest go through one `read_headers` call, then parse.
  5. Apply the calibrated-light and `ATH_REG`/`ATH_STK` skips, the same as `process_file` (:473, :492).
  6. Adopt through `find_moved_row`, else insert with: virtual path, `size` = uncompressed size, `modified_at` = entry time as UTC (2 s resolution), `archive_zip_path`, `archive_path_in_zip`, `archive_entry_crc32`, `format`.
  7. Recompute `frames.instrume` on adoption, as :545-584 does.
  8. Upsert `zip_archives` with stat, `frame_entries` and `scanned_at`.
  9. Log `info!(zip_path, frames, bytes_read, "zip scanned")`.
- `find_moved_row`:
  - SQL: the existing query (:506) plus `AND f.archive_zip_path IS NULL` when `!for_zip_entry`.
  - "Old location gone": for a disk row, `!Path::new(old_path).exists()`. For a zip-resident row (only when `for_zip_entry`), `!Path::new(archive_zip_path).exists()`.
  - The offline-root guard applies to the old location in both cases.
- The parallel walker runs zips one at a time after the parallel frame pass, emitting phase `"zips"` per zip with the zip's file name. Cancellation is checked between zips.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn store_and_deflate_zips_catalog_every_frame_with_virtual_paths() {
    // zip with Lights/L_001.fits, Lights/L_002.fits, notes.txt → 2 rows; path == virtual_path(zip, "Lights/L_001.fits");
    // archive_zip_path, archive_path_in_zip, archive_entry_crc32 set; header_fingerprint equals the disk file's
}
#[test] fn unchanged_zip_is_skipped_without_opening() { /* second scan_zip → skipped_unchanged, bytes_read == 0 */ }
#[test] fn changed_zip_keeps_known_rows_adds_new_and_leaves_vanished_missing() {
    // rebuild the zip with L_002 removed, L_003 added → L_001 row id unchanged, L_003 inserted, L_002 row still present
}
#[test] fn appledouble_entries_are_not_cataloged() { /* "__MACOSX/Lights/._L_001.fits" → no row */ }
#[test] fn calibrated_light_entries_are_skipped() { /* header with CALSTAT + ATH_CSRC → no row */ }
#[test] fn cataloged_frame_zipped_elsewhere_with_original_deleted_moves_into_the_zip() {
    // insert disk row (with an analysis row), delete the file, zip it → same files.id, path == virtual, markers set, analysis row kept
}
#[test] fn original_still_on_disk_makes_the_zip_copy_a_duplicate() { /* both rows exist, same fingerprint */ }
#[test] fn zip_moved_outside_the_app_is_adopted_by_its_new_location() { /* old zip removed, same zip at a new path → same row ids, new virtual paths */ }
#[test] fn released_entries_are_not_re_added() {}
#[test] fn zip_without_frames_is_recorded_with_zero_entries() {}
// scanner/mod.rs moved_file_guard_tests:
#[test] fn disk_copy_of_a_zip_resident_frame_never_takes_over_the_row() {
    // archived row (markers set, original path gone) + a new disk file with the same fingerprint → new row, archived row untouched
}
#[test] fn scanner_prunes_archive_staging_and_restore_temp() {}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core zip_store::scan moved_file_guard_tests app_metadata_dir_tests`. Expected: FAIL.
- [ ] **Step 3: Implement `scan_zip`, `virtual_path`, `find_moved_row`**, then switch both move-detection sites to `find_moved_row(conn, fp, path, false)`.
- [ ] **Step 4: Wire the zip pass into both walkers and add the `"zips"` phase label** in the frontend scan progress.
- [ ] **Step 5: Run** `cargo test -p athenaeum-core scanner zip_store` and `npx vitest run`. Expected: PASS.
- [ ] **Step 6: Commit** with `feat(zip): scanner reads zip metadata; frames move into zips; disk copies never hijack zip rows`.

### Task 5: Presence and hash jobs for zip-resident rows

**Files:**
- Create: `crates/athenaeum-core/src/zip_store/presence.rs`
- Modify:
  - `crates/athenaeum-core/src/api/scan_roots.rs:1566-1574` (`check_missing_files_in_scan_root`);
  - `crates/athenaeum-web/src/routes/missing_files.rs:116-124, 204-220`;
  - `crates/athenaeum-tauri/src/commands/missing_files.rs:153-174`;
  - `crates/athenaeum-core/src/duplicates/backfill.rs:105, 152, 364-380`.
- Test: `zip_store/presence.rs` `mod presence_tests`; `duplicates/backfill.rs` tests.

**Interfaces:**
- Produces:

```rust
/// Per call, opens each distinct zip's directory once.
pub struct ZipPresence { /* cache: zip path → Option<HashSet<entry>> */ }
impl ZipPresence { pub fn new() -> Self; pub fn is_present(&mut self, zip: &str, entry: &str) -> bool; }
pub fn row_is_present(presence: &mut ZipPresence, path: &str, archive_zip_path: Option<&str>, archive_path_in_zip: Option<&str>) -> bool;
```

- Behaviour:
  - All three missing-files sites drop `AND f.archived_in_operation IS NULL`. They select `archive_zip_path, archive_path_in_zip` and decide through `row_is_present`. The Tauri and web recheck use the same function, so the two copies cannot drift.
  - Backfill selects (:105, :152) and the master-hash pass (:364) add `AND archive_zip_path IS NULL` (the master-hash pass on `f`).

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn zip_resident_row_is_present_while_its_zip_lists_the_entry() {}
#[test] fn zip_resident_row_is_missing_when_the_zip_is_gone_and_is_never_deleted() {
    // check_missing_files_in_scan_root → row in missing_files, files row still there
}
#[test] fn content_index_and_master_hash_pass_skip_zip_resident_rows() { /* count_pending excludes them; fill_master_strong_hashes never opens the virtual path */ }
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core presence_tests backfill`. Expected: FAIL.
- [ ] **Step 3: Implement `ZipPresence` and switch the five SQL sites.**
- [ ] **Step 4: Run** `cargo test -p athenaeum-core` (missing-files and duplicates tests), `cargo test -p athenaeum-web`, `cargo check -p athenaeum-tauri`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat(zip): presence through the zip directory; hash jobs skip zip-resident rows`.

### Task 6: Cache and `materialize`

**Files:**
- Create: `crates/athenaeum-core/src/zip_store/cache.rs`, `zip_store/materialize.rs`
- Modify:
  - `crates/athenaeum-core/src/services/mod.rs` (`ServiceContext` gets `pub zip_cache: Arc<ZipCache>`, built in `new_for_tests` :142; update every struct literal: hosts `athenaeum-tauri/src/lib.rs:73` and `athenaeum-web/src/main.rs:188`, plus the test literals);
  - `crates/athenaeum-core/src/settings/mod.rs` (`keys::ARCHIVE_ZIP_CACHE_LIMIT_GB = "archive.zip_cache_limit_gb"`, `defaults::ARCHIVE_ZIP_CACHE_LIMIT_GB = 32`, listed in `defaults::all()`, getter beside `get_archive_compression` :643).
- Test: `zip_store/cache.rs` `mod cache_tests`, `zip_store/materialize.rs` `mod materialize_tests`

**Interfaces:**
- Consumes: Task 2 `extract_entry`; `duplicates::compute_xxhash`; `db::bank_strong_hash`.
- Produces:

```rust
pub struct ZipCache { /* dir, pins, in-flight map */ }
impl ZipCache {
    pub fn for_app_data(app_data_dir: &Path) -> Self;     // honours $ATHENAEUM_ZIP_CACHE
    pub fn status(&self) -> Result<ZipCacheStatus>;        // ZipCacheStatus { bytes, entries, limit_bytes } (camelCase, ts-exported)
    pub fn clear_unpinned(&self) -> Result<u64>;           // bytes removed
}
pub struct Pinned { /* drop releases the pin and runs eviction */ }
pub struct Materialized { /* catalog path → local path; holds the pins */ }
impl Materialized { pub fn local(&self, catalog_path: &str) -> &Path; }
pub fn materialize_paths(ctx: &ServiceContext, catalog_paths: &[String], emitter: &dyn ProgressEmitter,
                         progress_event: &str, cancel: &AtomicBool) -> Result<Materialized>;
pub fn materialize_one(ctx: &ServiceContext, catalog_path: &str) -> Result<(PathBuf, Option<Pinned>)>;
pub struct ExtractionEstimate { pub files: u64, pub bytes: u64 }  // camelCase, ts-exported
pub fn estimate_extraction(ctx: &ServiceContext, file_ids: &[i64]) -> Result<ExtractionEstimate>;
```

- Cache rules:
  - The key is an xxh3 hex of `zip path \0 entry \0 zip size \0 zip mtime \0 crc32`. The local path is `<cache>/<key>/<entry file name>`.
  - A hit refreshes the dir's mtime (used for LRU). Concurrent requests for one key wait on one in-flight extraction (`Mutex<HashMap<key, Arc<OnceLock<Result<PathBuf, String>>>>>`).
  - Eviction runs on pin release: remove least-recently-used unpinned key dirs until the total is ≤ the limit read from settings. Pinned dirs are never removed, so an operation larger than the limit still runs.
- `materialize_paths`:
  - Splits paths into disk and zip-resident by one `SELECT path, archive_zip_path, archive_path_in_zip, archive_entry_crc32, id FROM files WHERE path IN (…)`.
  - Extracts zip-resident misses with progress `{current, total, message: entry file name}` on `progress_event`.
  - After each extraction it banks `strong_hash` (when NULL) and `content_hash` (when NULL, `compute_xxhash` on the cached file).
  - A per-entry failure is an `Err` naming the frame and the zip ("inside M31.zip, which is not reachable" when the zip is missing). Callers that tolerate per-frame failure call `materialize_one` per frame instead.
- Logging: `debug!(zip_path, zip_entry, cache_hit, bytes_read, "zip entry materialized")`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn disk_rows_and_unknown_paths_pass_through_untouched() {}
#[test] fn zip_row_is_extracted_once_and_reused() { /* second materialize → cache_hit, no new bytes read */ }
#[test] fn concurrent_requests_for_one_entry_extract_once() { /* 4 threads → one extraction (count via a test hook on ZipCache) */ }
#[test] fn eviction_never_removes_a_pinned_entry_and_restores_the_limit_after_release() {
    // limit 1 byte: materialize 3 entries in one call → all 3 present while held; drop → cache total <= limit
}
#[test] fn same_entry_path_in_two_zips_caches_separately() {}
#[test] fn extraction_of_a_vanished_zip_fails_the_frame_and_leaves_no_partial_file() {
    // catalog the zip, delete it → materialize_one Err contains "not reachable"; cache dir has no *.partial
}
#[test] fn extraction_banks_strong_and_content_hash() {}
#[test] fn estimate_counts_only_uncached_zip_resident_rows() {}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core zip_store::cache zip_store::materialize`. Expected: FAIL.
- [ ] **Step 3: Implement the cache, the setting, the `ServiceContext` field, and `materialize_*` / `estimate_extraction`.**
- [ ] **Step 4: Run** `cargo test -p athenaeum-core zip_store settings`, `cargo check -p athenaeum-web -p athenaeum-tauri --tests`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat(zip): extraction cache with pins and LRU; materialize catalog paths`.

### Task 7: Consumers — preview, plate solve, analysis, duplicates, blink

**Files:**
- Modify:
  - preview: `crates/athenaeum-tauri/src/commands_rustafits.rs:36` (`read_fits_image_bytes`, resolve the path before `preview_cache_key` at :94); `crates/athenaeum-tauri/src/commands/files.rs:149`; `commands/collab.rs:744`; `crates/athenaeum-web/src/routes/images.rs:49-73` (`render_path_jpeg`);
  - plate solve: `crates/athenaeum-tauri/src/commands/plate_solve.rs:168` and `crates/athenaeum-web/src/routes/plate_solve.rs:194` (per item, keep the `Pinned` alive across `solve_frame_with_hints`);
  - analysis: `crates/athenaeum-core/src/api/analysis.rs:240-313` (`analyze_frame_set`), :498 (`get_frame_star_metrics`), :587 (`compute_flat_contour_plot`);
  - duplicates and blink: `crates/athenaeum-core/src/api/files.rs:1297` (`verify_duplicate_pair`); `crates/athenaeum-core/src/api/collab_blink.rs:93-107` (`raw_path_of`: present = disk exists OR zip presence).
- Test: `api/analysis.rs` tests, `api/files.rs` tests, `collab_blink.rs` tests, web `routes/images.rs` test

**Interfaces:**
- Consumes: Task 6 `materialize_one`, `materialize_paths`.
- Rule: each per-frame consumer calls `materialize_one(ctx, &path)` at the line where it currently uses `path`. A failure becomes that frame's existing failure (analysis warning, plate-solve error event). `analyze_frame_set` calls `materialize_one` inside its worker per frame, so one unreachable zip fails only its frames.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn analyze_frame_set_measures_a_zip_resident_frame() { /* catalog a zipped frame → analysis row written */ }
#[test] fn analyze_frame_set_reports_unreachable_zip_per_frame_and_finishes_the_rest() {}
#[test] fn verify_duplicate_pair_compares_disk_and_zip_copies() { /* byte-identical → verified */ }
#[test] fn blink_raw_path_of_resolves_a_zip_resident_frame() {}
// web: #[tokio::test] async fn get_frame_preview_renders_a_zip_resident_frame() {}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core analysis files collab_blink` and `cargo test -p athenaeum-web images`. Expected: FAIL.
- [ ] **Step 3: Thread `materialize_one` through each site in the list.**
- [ ] **Step 4: Run the same commands plus** `cargo check -p athenaeum-tauri`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat(zip): preview, plate solve, analysis, duplicate verify and blink read zip-resident frames`.

### Task 8: Consumers — stacking and masters

**Files:**
- Modify:
  - stacking plan: `crates/athenaeum-core/src/stacking/plan.rs:2224-2247`. Its `masters_unrebuildable` blocker comes from `api/lights.rs:640`; it must count only masters neither on disk nor zip-present (via `row_is_present`). Also `plan.rs:2322` (`probe_file` on `landed_path`: resolve through `materialize_one` when the landed path is a catalog row).
  - stacking run: `crates/athenaeum-core/src/stacking/run.rs:2444-2467` (`resolve_prepare_job`: `materialize_one` before `probe_file`; keep the `Pinned` in the prepare job until it ends), `run.rs:2986` (`resolve_frame_inputs`), `stacking/project_source.rs:274-285`, `stacking/solve.rs:300`.
  - masters: `crates/athenaeum-core/src/api/masters.rs:689-720` (`check_rebuild_source_ready`: zip-present counts as ready), `:1150` (`run_build`: `materialize_paths(ctx, &paths, emitter, "master-build-progress", cancel)` before integration); `crates/athenaeum-core/src/api/lights.rs:1014` (`classify_raw_set_buildability`).
- Test: `stacking/plan.rs` tests, `api/masters.rs` tests

**Interfaces:**
- Consumes: Task 5 `row_is_present`; Task 6 `materialize_one`, `materialize_paths`.
- Copy that changes: the "restore from archive first" texts at `plan.rs:2224-2247`, `masters.rs:689-720` and `lights.rs:1014` now fire only when the bytes are reachable nowhere. They read "{n} master file(s) missing on disk and in zip files". The `"masterFiles"` code is kept.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn stacking_plan_has_no_master_blocker_when_masters_are_zip_resident() {}
#[test] fn stacking_plan_still_blocks_when_a_master_is_in_a_missing_zip() {}
#[test] fn rebuild_source_ready_accepts_zipped_originals() {}
#[test] fn master_build_from_zip_resident_frames_matches_the_disk_build() {
    // same frames on disk vs zipped: integrated master bytes identical (feature-gated: #[cfg(all(feature="render", feature="solver"))])
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core stacking masters lights`. Expected: FAIL.
- [ ] **Step 3: Thread the calls and rewrite the blocker conditions.**
- [ ] **Step 4: Run** `cargo test -p athenaeum-core` (all targets; this touches the stacking suite). Expected: PASS.
- [ ] **Step 5: Commit** with `feat(zip): stacking and master builds read zip-resident frames; archive blockers only when bytes are unreachable`.

### Task 9: Consumers — export and calibrated lights

**Files:**
- Modify:
  - `crates/athenaeum-core/src/export/file_organizer.rs:583-676` (`organize_files_wbpp`), :833 (`copy_or_link`): `materialize_one` per placement before `copy_or_link`. With `use_symlinks` and a zip-resident source, copy instead of linking and count it in a new `ExportResult.zip_copied: usize`.
  - `export/project_organizer.rs:363` (`place_one`, same rule);
  - `export/calibrated_generator.rs:560`; `calibration_library/light_resolve.rs:72-80` (`resolve_master`), :376-385 (`resolve_frame_inputs`);
  - `export/data_collector.rs:545` (a zip-present raw node is not missing), :612;
  - `api/lights.rs:330-396` (`check_mode_ready`), :477-563 (`readiness_from_data`).
  - transfers, sync and collab (spec §7.9, v1 stays ineligible, but with the right reason): `api/sync.rs:3449` (`plan_selection_package`), `api/sync_prepare.rs:776`, `api/collab.rs:4210, 5534`. A zip-resident file's ineligibility reason becomes `"inside a zip — unpack it first"` instead of "file missing on disk" or "archived or moved".
- Test: `export/file_organizer.rs` tests, `api/lights.rs` tests

**Interfaces:**
- Consumes: Task 5 `row_is_present`; Task 6 `materialize_one`.
- Produces: `ExportResult.zip_copied: usize` (camelCase `zipCopied`, ts-exported) for Task 15's dialog line.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn wbpp_export_copies_zip_resident_frames_even_in_symlink_mode() {
    // use_symlinks=true → dest is a regular file, not a symlink; zip_copied == 1; disk frame still symlinked
}
#[test] fn export_readiness_counts_zip_present_raw_calibration_as_available() {}
#[test] fn calibrated_lights_generate_from_a_zip_resident_light_and_master() {}
#[test] fn sync_selection_marks_zip_resident_files_ineligible_with_the_unpack_reason() {}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core export lights calibration_library sync`. Expected: FAIL.
- [ ] **Step 3: Thread the calls, add `zip_copied`, and fix the readiness checks.**
- [ ] **Step 4: Run** `cargo test -p athenaeum-core` and `cargo check -p athenaeum-web -p athenaeum-tauri`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat(zip): export and calibrated lights read zip-resident frames; link mode copies them`.

### Task 10: Placing entries — permanent unpack and Unarchive

**Files:**
- Create: `crates/athenaeum-core/src/zip_store/place.rs`, `crates/athenaeum-core/src/api/zip.rs`
- Modify:
  - `crates/athenaeum-core/src/archive/restore.rs:158` (`run_restore_inner`): extract only entries whose target is missing, through `place_entry`. Keep the hash check against `expected_hash`, the conflicts and the `keep_zip_after_restore` behaviour. When the zip is kept, record restored entries in `zip_released_entries`.
  - Commands: `crates/athenaeum-tauri/src/commands/zip.rs` (new, re-exported from `commands/mod.rs`, registered in `invoke_handler!`) and `crates/athenaeum-web/src/routes/zip.rs` (new, registered in `build_router`).
- Test: `zip_store/place.rs` tests, `tests/archive_e2e.rs` (extend)

**Interfaces:**
- Consumes: Task 2 `extract_entry`, `is_safe_entry_name`.
- Produces:

```rust
pub enum PlaceOutcome { Placed, AdoptedExisting, Conflict(String) }
pub fn place_entry(zip: &Path, entry: &str, expected_crc: Option<u32>, dest: &Path, cancel: &AtomicBool) -> Result<PlaceOutcome>;
// never overwrites: existing dest with identical bytes (CRC32 of the file == expected) → AdoptedExisting, otherwise Conflict
#[serde(rename_all = "camelCase")] pub enum UnpackTarget { FrameSet { id: i64 }, CalibrationSet { id: i64 } }
pub struct UnpackOutcome { pub unpacked: usize, pub adopted: usize, pub conflicts: Vec<String> } // camelCase, ts-exported
pub fn unpack_zip_frames(ctx: &ServiceContext, policy: &PathPolicy, target: UnpackTarget, dest_dir: &Path,
                         emitter: &dyn ProgressEmitter, cancel: &AtomicBool) -> Result<UnpackOutcome>;
```

- `unpack_zip_frames`:
  - Covers foreign zip-resident rows of the set; our rows go through Unarchive.
  - The destination is `dest_dir/<entry path>`, with entries that fail `is_safe_entry_name` refused.
  - After each placement, one transaction: `files.path` = dest, `filename`, `modified_at` from disk, zip markers and CRC cleared, and `zip_released_entries` insert.
  - Progress `zip-unpack-progress {current, total, message}`; finish `archive-finished {operation_id: 0, outcome, kind: "unpack"}`.
- Commands: `unpack_zip_frames(target, destDir)` on both hosts, with policy-checked `dest_dir`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn unpack_moves_rows_to_disk_with_metadata_intact() { /* analysis + plate-solve rows survive; rescan of the zip adds nothing */ }
#[test] fn unpack_adopts_an_identical_existing_file_and_reports_a_different_one_as_conflict() {}
#[test] fn unpack_never_writes_outside_the_target() { /* row with archive_path_in_zip "../evil.fits" forced in → refused, nothing outside dest_dir */ }
// tests/archive_e2e.rs:
#[test] fn unarchive_after_deleting_two_restored_files_extracts_exactly_those_two() {
    // archive → restore (keep zip) → delete 2 restored files → re-run restore path → extraction count == 2
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core place unpack --test archive_e2e`. Expected: FAIL.
- [ ] **Step 3: Implement `place_entry`, `unpack_zip_frames`, the restore rewrite, and both command wrappers.**
- [ ] **Step 4: Run** `cargo test -p athenaeum-core` (all targets), `cargo test -p athenaeum-web`, `cargo check -p athenaeum-tauri`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat(zip): permanent unpack; Unarchive extracts only missing entries`.

### Task 11: Our Move and ZIP on the new model

**Files:**
- Modify:
  - `crates/athenaeum-core/src/archive/executor.rs:471` (`finalize_phase`): after marking files, upsert `zip_archives` for each built zip (`created_by_operation = operation_id`) and write each moved row's `archive_entry_crc32` from `list_entries` of that zip.
  - `crates/athenaeum-core/src/archive/planner.rs:65-165` (`build_plan`) and :291-375 (`build_calibration_set_plan`): skip zip-resident members instead of erroring, for lights and the calibration chain alike. Replace the "already has archived member file(s)" bail for zip-resident members with a skip. Bail `"all frames are already in zip files"` when no member is on disk.
- Test: `archive/planner.rs` tests, `archive/executor.rs` tests

**Interfaces:**
- Consumes: Task 2 `list_entries`. Our rows keep their original `files.path`; only foreign rows use virtual paths.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn move_and_zip_skips_lights_already_in_a_foreign_zip() { /* half the lights foreign zip-resident → plan files == disk half only */ }
#[test] fn move_and_zip_with_every_frame_in_zips_reports_all_frames_already_in_zip_files() {}
#[test] fn finalize_records_the_zip_and_entry_crcs() { /* run_operation → zip_archives row with created_by_operation; every moved row has archive_entry_crc32 */ }
#[test] fn rescan_of_our_archive_zip_adds_no_rows() { /* scan_zip over the built zip → inserted empty */ }
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core archive`. Expected: FAIL.
- [ ] **Step 3: Implement the planner skips and the finalize writes.**
- [ ] **Step 4: Run** `cargo test -p athenaeum-core archive --test archive_e2e`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat(zip): Move and ZIP zips only disk frames and records its zips in the zip catalog`.

### Task 12: Zips as whole files — browser listing, rename, move, relink

**Files:**
- Modify:
  - listing: `crates/athenaeum-core/src/api/files.rs:35` (`DirectoryContents` gains `zips: Vec<ZipListItem>`), :244 (`get_directory_contents`) and :453 (`get_camera_directory_contents`);
  - rename: `api/files.rs:905-972` (`rename_path`);
  - move: `crates/athenaeum-core/src/file_op/executor.rs:498` (`sync_catalog_path`);
  - relink: `crates/athenaeum-core/src/relinking/mod.rs:33, 206, 278` and `api/scan_roots.rs:1415` (`relink_scan_root`).
- Create: in `crates/athenaeum-core/src/zip_store/catalog.rs`, `rename_zip` and `rename_zip_prefix`.
- Modify: `crates/athenaeum-core/src/api/zip.rs` (`list_zip_entries`), plus Tauri `commands/zip.rs` and web `routes/zip.rs` wrappers.
- Test: `zip_store/catalog.rs` tests, `api/files.rs` tests, `relinking/mod.rs` tests

**Interfaces:**
- Produces:

```rust
pub struct ZipListItem { pub path: String, pub size: i64, pub frame_entries: i64 }   // camelCase, ts-exported
pub struct ZipDirListing { pub subdirectories: Vec<String>, pub files: Vec<FileWithFrame>,
                           pub other_entries: Vec<ZipOtherEntry> }                    // ZipOtherEntry { name, size }
pub fn list_zip_entries(ctx: &ServiceContext, zip_path: &str, inner_dir: &str) -> Result<ZipDirListing, ApiError>;
pub fn rename_zip(conn: &Connection, old_zip: &str, new_zip: &str) -> Result<usize>;
pub fn rename_zip_prefix(conn: &Connection, old_prefix: &str, new_prefix: &str) -> Result<usize>;
```

- Behaviour:
  - `get_directory_contents` lists `zip_archives` rows whose parent dir equals the directory and that have `frame_entries > 0`.
  - `list_zip_entries` reads the directory live (`list_entries`). Cataloged entries come from `files` by virtual path or by `archive_zip_path` + `archive_path_in_zip`. It returns one level under `inner_dir`.
  - `rename_zip` updates `zip_archives.path`, `files.archive_zip_path`, and the virtual `files.path` of foreign rows by the separator-strict prefix `old_zip + MAIN_SEPARATOR`.
  - `rename_zip_prefix` does the same for every zip under a directory prefix. `rename_path` calls it for a directory (next to `rename_files_path_prefix`) and calls `rename_zip` for a zip file. `sync_catalog_path` calls `rename_zip` when the moved file is a known zip.
  - Relink: zip-resident rows are excluded from the fingerprint map and from orphan detection. `relink_scan_root` calls `rename_zip_prefix(old_root, new_root)` for zips that exist at the new location.
- Commands: `list_zip_entries(zipPath, innerDir)` on both hosts.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn directory_listing_shows_zips_with_frames_only() {}
#[test] fn list_zip_entries_returns_one_level_with_catalog_metadata() {}
#[test] fn renaming_a_zip_keeps_its_frames_present() { /* rename_path on the zip → virtual paths follow; row_is_present true */ }
#[test] fn renaming_a_folder_moves_foreign_and_our_zip_paths() { /* our row keeps files.path (original), archive_zip_path follows */ }
#[test] fn moving_a_zip_with_file_op_keeps_its_frames_present() {}
#[test] fn relink_never_reports_zip_resident_rows_as_orphans() {}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core zip_store::catalog files relinking file_op`. Expected: FAIL.
- [ ] **Step 3: Implement the listing, the two rename functions, and the call sites.**
- [ ] **Step 4: Run** `cargo test -p athenaeum-core`, `cargo test -p athenaeum-web`, `cargo check -p athenaeum-tauri`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat(zip): zips listed, renamed, moved and relinked as whole files`.

### Task 13: Black Hole for zips

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations_blackhole.rs`:
  - `add_to_black_hole` (:87) and `bulk_move_to_black_hole` (:138) refuse zip-resident rows;
  - new zip functions beside them.
- Modify: Tauri `commands/duplicates.rs` and web `routes/duplicates.rs` (four new commands, both hosts).
- Test: `db/operations_blackhole.rs` tests

**Interfaces:**
- Produces:

```rust
pub fn add_zip_to_black_hole(conn: &Connection, zip_path: &str, from_where: &str) -> Result<i64>;
pub struct BlackHoleZipEntry { pub id: i64, pub zip_path: String, pub size: i64, pub frame_entries: i64,
                               pub archive_operation_id: Option<i64>, pub from_where: String, pub moved_at: String } // camelCase, ts-exported
pub fn get_black_hole_zips(conn: &Connection) -> Result<Vec<BlackHoleZipEntry>>;
pub fn restore_zip_from_black_hole(conn: &Connection, id: i64) -> Result<()>;
pub fn send_zip_to_void(conn: &Connection, id: i64) -> Result<usize>;   // rows deleted
```

- Behaviour:
  - Refusal text: `"this frame is inside {zip file name}; only the whole zip can be removed"`. Bulk mode puts the refused rows in `BulkMoveResult` failures, not an abort.
  - `send_zip_to_void` runs one savepoint: `unregister_master_if_any` for each row, delete every `files` row with `archive_zip_path = zip`, delete the `zip_archives` row (cascading the released entries and the black-hole row), commit, then `fs::remove_file(zip)` if it exists. The removal mirrors `send_to_void` (:367).
- Commands: `move_zip_to_black_hole(zipPath, fromWhere)`, `get_black_hole_zips`, `restore_zip_from_black_hole(id)`, `send_zip_to_void(id)`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn single_zip_resident_row_is_refused_by_black_hole() {}
#[test] fn bulk_move_reports_zip_rows_as_failures_and_moves_the_rest() {}
#[test] fn void_of_a_zip_deletes_the_zip_and_all_its_rows() {}
#[test] fn restore_of_a_zip_from_black_hole_removes_only_the_black_hole_row() {}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core operations_blackhole`. Expected: FAIL.
- [ ] **Step 3: Implement the functions and the four commands on both hosts.**
- [ ] **Step 4: Run** `cargo test -p athenaeum-core`, `cargo test -p athenaeum-web`, `cargo check -p athenaeum-tauri`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat(zip): Black Hole and Void for a whole zip; single entries refused`.

### Task 14: Frontend — browser zip view, Black Hole zips, cache settings

**Files:**
- Modify:
  - browser: `src/components/dualpane/types.ts:23` (`DirectoryListing` gains `zips`); `src/components/dualpane/DualPaneFileBrowser.tsx`:
    - zip rows next to subdirectories (:1978);
    - double-click or `activatePath` (:941) enters a zip;
    - `PaneState` (types.ts:10) gains `zip?: { path: string; innerDir: string }`;
    - while inside a zip, the listing comes from `list_zip_entries`, and F8 (:1011), rename, move and drag are disabled with a status-line reason;
    - F8 on a zip row calls `move_zip_to_black_hole` after a confirmation through `DialogShell` that shows `frameEntries`, plus "This is the only copy of an archived frame set" when `createdByOperation`;
    - reveal (:798-815): when the path is a virtual path under a known zip, open the zip and select the entry.
  - Black Hole: `src/pages/BlackHole.tsx` (a "Zip archives" group from `get_black_hole_zips`, with restore and void).
  - settings: `src/settings/registry.ts:180` (`general.archive` gains `zipCacheLimitGb`) and `src/components/settings/sections/ArchiveSection.tsx` (limit input plus size and "Clear cache", following `SkyBackgroundSection.tsx:163-210`).
  - Tauri `commands/zip.rs` and web `routes/zip.rs`: `get_zip_cache_status`, `clear_zip_cache`.
  - `src/types/models.ts` (generated types from `ts_contract`).
- Test: `src/components/dualpane/*.test.tsx` (vitest), `src/settings/registry.test.ts`

**Interfaces:**
- Consumes:
  - Task 12: `list_zip_entries`, `DirectoryContents.zips`.
  - Task 13: the Black Hole zip commands.
  - Task 6: `ZipCacheStatus`.

- [ ] **Step 1: Write the failing vitest tests**

```ts
it('enters a zip on double-click and lists its entries read-only', ...);   // F8 inside the zip → no enqueueDelete call
it('F8 on a zip row asks for confirmation with the frame count', ...);
it('registry lists zipCacheLimitGb in general.archive', ...);
```

- [ ] **Step 2: Run** `npx vitest run src/components/dualpane src/settings`. Expected: FAIL.
- [ ] **Step 3: Implement the UI and the two cache commands on both hosts.**
- [ ] **Step 4: Run** `npx vitest run`, `npm run build`, `cargo test -p athenaeum-web`, `cargo check -p athenaeum-tauri`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat(zip): browse inside zips, Black Hole zips, cache size and clear in Settings`.

### Task 15: Frontend — zip chips, Unpack, extraction lines

**Files:**
- Modify:
  - Objects: the backend DTO behind `filteredFrameSets` (the struct whose `member_count` Objects.tsx:1096 reads) gains `zip_member_count: i64`, counting rows with `archive_zip_path IS NOT NULL`. The chip goes in `src/pages/Objects.tsx` ~:1165 (title row) and `src/components/ObjectsTableView.tsx`: `<Chip tone="info">zip</Chip>` when all members are in zips, else `` `${zip} of ${total} in zip` ``.
  - FrameSetDetail: `src/pages/FrameSetDetail.tsx`:
    - zip chip on zip-resident rows in the frame tables;
    - an "Unpack from zip" button whenever the set has foreign zip-resident frames;
    - an unpack dialog (`DialogShell`, a folder picker through `api`, default `<zip parent>/<zip stem>`, progress on `zip-unpack-progress`, finish on `archive-finished` kind `"unpack"`).
  - Locate: on a zip-resident row it navigates with `reveal: { path: file.path }` (handled in Task 14).
  - Extraction lines: the stacking plan dialog, the WBPP export dialog (`ExportTab.tsx`), the batch plate solve start, and analysis start call `estimate_zip_extraction(fileIds)` and show `From zip: ${files} files, ${gb} GB` when `files > 0`. The export dialog adds "Zip-resident frames are copied, not linked" when `useSymlinks` is on.
  - Commands: Tauri `commands/zip.rs` and web `routes/zip.rs` get `estimate_zip_extraction(fileIds)`.
- Test: vitest for the chip helper and the extraction line formatter

**Interfaces:**
- Consumes:
  - Task 6: `ExtractionEstimate`.
  - Task 9: `ExportResult.zipCopied`.
  - Task 10: `unpack_zip_frames`, `UnpackOutcome`.

- [ ] **Step 1: Write the failing tests**

```ts
it('zipChipLabel', () => { expect(zipChipLabel(5, 5)).toBe('zip'); expect(zipChipLabel(2, 5)).toBe('2 of 5 in zip'); expect(zipChipLabel(0, 5)).toBeNull(); });
it('extractionLine', () => { expect(extractionLine({ files: 120, bytes: 6.2e9 })).toBe('From zip: 120 files, 6.2 GB'); expect(extractionLine({ files: 0, bytes: 0 })).toBeNull(); });
```

- [ ] **Step 2: Run** `npx vitest run`. Expected: FAIL.
- [ ] **Step 3: Implement the DTO field, the components, the dialog lines and the estimate command on both hosts.**
- [ ] **Step 4: Run** `npx vitest run`, `npm run build`, `cargo test -p athenaeum-core`, `cargo test -p athenaeum-web`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat(zip): zip chips, Unpack from zip, extraction size in batch dialogs`.

### Task 16: Docs, command count, acceptance

**Files:**
- Modify:
  - architecture: `docs/architecture/commands.md` (316 → 325, with the recount command, the history line and the new `zip` module in the list); `CLAUDE.md` (module map gets `zip_store`; command count 325 in 26 modules);
  - logging: `docs/logging/README.md` (fields `zip_path`, `zip_entry`, `bytes_read`, `cache_hit`);
  - subsystem references: `docs/archive/README.md` (the new model, planner skip, partial Unarchive); `docs/catalog/README.md` (zip-resident rows, virtual paths, presence, hash jobs); `docs/file-browser/README.md` (zip view, whole-zip rename/move/Black Hole);
  - `docs/superpowers/open-items.md` (the NAS smokes below, as owed).
- Run: the gate and the acceptance smokes.

- [ ] **Step 1: Recount the commands.**

```bash
grep -rE '^\s*#\[tauri::command(\]|\()' crates/athenaeum-tauri/src/commands/*.rs | wc -l
```

Expected: `325`.
- [ ] **Step 2: Update the docs listed above.**
- [ ] **Step 3: Run the gate:** `cargo test -p athenaeum-core` (all targets), `cargo test --workspace`, `npm run build`, `npx vitest run`. Expected: all green.
- [ ] **Step 4: Run the NAS acceptance** (spec §13) on the dev build with the real dev DB (`npm run tauri dev`), with a zip of a full night on Universe. The owner approves the write before any file goes to the share.
  - Network reads for the metadata scan are at most 64 KiB per entry plus the directory, and under 0.5% of the zip size (from the `zip scanned` log line, `bytes_read`).
  - A rescan of the unchanged zip shows no `bytes_read`.
  - The frame set shows in Objects with the zip chip; preview, plate solve, analysis, a stack and a WBPP export (copy and link) run; link mode copies.
  - Unpack, and a Move and ZIP set stacked without Unarchive.
  - The share unmounted: rows missing, nothing deleted. Remounting brings them back.
  - Extraction runs at no less than half the share's sequential read rate.
  - The same checks in the web build (Docker, NAS as a volume).
  - Record each smoke that passes. One not run stays in `docs/superpowers/open-items.md`.
- [ ] **Step 5: Commit** with `docs(zip): commands, logging fields, archive/catalog/file-browser references; acceptance record`.
