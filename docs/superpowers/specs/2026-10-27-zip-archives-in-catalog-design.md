# ZIP archives in the catalog — design

Date: 2026-10-27. Status: draft for owner approval.

## 1. Intent

Astrophotographers keep finished nights as `.zip` files on a NAS: lights, darks, flats, bias, often years of them. A
user reported that Athenaeum cannot see them: the scanner takes only `fits`/`fit`/`fts`/`xisf`, so the only way into
the catalog is to unzip tens or hundreds of gigabytes first.

A ZIP has a central directory at its end (every entry's name, sizes, CRC32 and offset), and a FITS or XISF header sits
at the start of the file. So a zip can be cataloged by reading the directory and the first few tens of kilobytes of
each frame entry, without unzipping anything. For a 20 GB zip of 500 lights that is about 15 MB of network reads.

The design has three parts, and they are one model, not three features:

1. **A zip is a file type in the catalog.** The scanner reads a zip's metadata only. Its frames join the library like any
   other frame: clustering into frame sets, calibration matching, the Objects page, sky chart, duplicates. The file
   browser opens a zip like a folder and shows its structure and the metadata of its frames.
2. **Data is extracted on demand.** When an operation needs pixels (preview, plate solve, analysis, stacking, export,
   master build) only the entries it needs are extracted: into a size-limited local cache, or permanently into a folder
   the user picks.
3. **Our own Archive feature moves onto the same model.** A frame set zipped by Move and ZIP holds the same kind of
   "file in a zip" rows as a scanned foreign zip. Today such a set can only be used after a full Unarchive. After this
   change every operation works on it through the same extraction, and Unarchive extracts only what is missing.

## 2. Goals

- Scanning a folder with zips catalogs every FITS/XISF entry in them, reading only the central directory and each
  entry's header bytes. An unchanged zip on a later scan costs one `stat`.
- Frames inside zips behave like on-disk frames for everything that needs only metadata.
- A frame that is already cataloged and was zipped outside the app (original deleted) moves into the zip: its row,
  analysis, plate solve and links are kept.
- Every operation that needs pixels or whole-file bytes gets them through one core primitive ("materialize"), which
  extracts zip entries to the cache and hands back local paths. No consumer opens a zip itself.
- The user can permanently unpack the zipped frames of a frame set into a folder; the catalog rows keep all their
  metadata and point at the new files.
- In the dual-pane file browser a zip opens like a folder (read-only). The zip as a whole can be moved, renamed and
  sent to the Black Hole.
- Our Move and ZIP sets use the same rows, the same materialize step and the same extraction code. Unarchive extracts
  only the missing entries, not every entry of every zip.

## 3. Non-goals (v1)

- Other container formats: 7z, RAR, tar, tar.gz. Solid 7z/RAR and tar.gz have no random access, so a frame can only be
  reached by decompressing everything before it. Split zips (`.z01`…), encrypted entries and zips inside zips are
  skipped with a log line.
- Writing into any zip, ours or foreign: no add, no delete, no rename of an entry, no update in place.
- Reading Stored (uncompressed) entries in place without a cache copy. This is a later optimization; v1 always extracts.
- Sending zip-resident frames through transfers, personal sync or collab publish (see §7.9).
- Changing how Move and ZIP builds a zip. The only planner change is §7.7 (skip frames already in a zip).

## 4. Anti-goals: a failure even if it works

- A metadata scan that reads whole zips over the network.
- Any write to a user's zip, or deleting a user's zip as a side effect of an Athenaeum action. Only an explicit Send to
  Void of the zip deletes it.
- Deleting catalog rows because a zip is missing. A NAS share that is not mounted looks the same as a deleted zip, and
  the scanner never deletes rows for missing files ("no orphan cleanup").
- Two representations of "a file in a zip": one for our archive and one for foreign zips.
- A consumer that opens `files.path` of a zip-resident row and reports "file not found", or an export that leaves a
  symlink pointing into the cache.
- Silent cache eviction of a file an operation is still reading.

## 5. Owner decisions (2026-10-27)

| Question | Decision |
| ---- | ---- |
| A frame exists both on disk and in a zip | The zip copy is a separate row, a duplicate exactly as two on-disk copies are today; the Duplicates page shows the pair. |
| A cataloged frame was zipped outside the app and its original deleted | The frame moves into the zip: the existing row becomes zip-resident and keeps its analysis, plate solve, frame-set membership and calibration links, the same way the scanner follows a file moved between folders. |
| Where a frame set whose frames are all in a foreign zip appears | In Objects, as a normal frame set, with a "zip" badge. The user can Move to Archive it like any other set. |
| Move and ZIP on a set whose frames are partly or wholly in a foreign zip | Only the frames on disk are zipped; frames already in a zip count as archived and are skipped. |
| The foreign zip after a permanent unpack | Always kept. Athenaeum never deletes it on its own. |
| Zips in the file browser | A zip opens like a folder: its structure and the metadata of its frames are visible. Single entries cannot be deleted; only the whole zip can (Black Hole → Void). |
| Warning before extraction into the cache | Single-frame actions (preview, one plate solve, header view) extract without asking. Batch operations (stack, export, batch plate solve, analysis, master build) show "From zip: N files, X GB" in their existing start or plan dialog. No extra modal. |

## 6. Catalog model

### 6.1 One kind of zip-resident row

A `files` row is **zip-resident** when `archive_zip_path IS NOT NULL`. The bytes live at entry `archive_path_in_zip` of
the zip at `archive_zip_path`. There is no file at `files.path`.

What `files.path` holds for a zip-resident row:

| Origin | `archived_in_operation` | `files.path` |
| ---- | ---- | ---- |
| Our Move and ZIP | set | the original disk location (the restore target), as today |
| A scanned foreign zip, or a frame that moved into one (§7.2) | NULL | a virtual path `<zip path>/<entry path>` (e.g. `/Volumes/NAS/2024/M31.zip/Lights/L_001.fits`) |

The virtual path keeps `files.path NOT NULL UNIQUE` true (no real file can sit under a file), reads naturally in logs,
matches what the file browser shows inside the zip, and survives a folder rename through the existing
separator-strict prefix swap. `files.format` stays `FITS`/`XISF` (the entry's format); `files.size` is the entry's
uncompressed size; `files.modified_at` is the entry's ZIP timestamp.

Core gets one helper that every consumer goes through:

```rust
pub enum FileLocation { Disk(PathBuf), Zip { zip: PathBuf, entry: String, crc32: Option<u32>, size: u64 } }
pub fn file_location(row: &FileRow) -> FileLocation;
```

### 6.2 New tables and columns

`zip_archives`, one row per zip the scanner has read or Move and ZIP has built:

| Column | Meaning |
| ---- | ---- |
| `id` | key |
| `path` | absolute zip path, UNIQUE |
| `size`, `modified_at` | the zip's stat at the last read; equal stat on a later scan means skip |
| `frame_entries` | count of FITS/XISF entries cataloged |
| `scanned_at` | last central-directory read |
| `created_by_operation` | `archive_operations.id` for our zips, NULL for foreign |

Zips with no frame entries get a row too (so the next scan skips them by stat), with `frame_entries = 0`.

`zip_released_entries (zip_id, entry_path)`: entries whose row was moved to disk by a permanent unpack or an
Unarchive that kept the zip. A rescan of the zip does not re-add them (§7.6).

`files.archive_entry_crc32`: the entry's CRC32, so extraction verifies without re-reading the directory and the cache
key is stable.

Black Hole gets a zip target: a Black Hole item is either a `files` row (as today) or a `zip_archives` row. The exact
schema shape (nullable `zip_archive_id` beside `file_id`, or a sibling table) is the builder's choice.

Migrations backfill `zip_archives` rows for existing Move and ZIP zips from `archive_operation_files`. The CRC32 column
is filled from each zip's directory lazily (a missing zip leaves it NULL; extraction then verifies by `expected_hash`
for our rows).

### 6.3 Presence

A zip-resident row is present when its zip exists and the entry is listed in its directory. A missing zip makes its rows
missing (the existing missing-files machinery), never deleted. The missing-files check today excludes
`archived_in_operation IS NOT NULL` in two copies (`api/scan_roots.rs:1576` and the web route
`athenaeum-web/src/routes/missing_files.rs:123, 200-204`); both change to "zip-resident rows are checked through
their zip".

## 7. Behaviour

### 7.1 Scanning a zip

- Both walkers take `.zip` beside `FRAME_EXTENSIONS`. They prune `.athenaeum`, and now also `.athenaeum_staging` and
  `.athenaeum_restore_temp` (an archive root may sit inside a scan root, so our temp copies must never be cataloged).
- For each zip: stat. Same `size` + `modified_at` as its `zip_archives` row → skip. Otherwise read the central
  directory.
- For each entry with a frame extension (case-insensitive):
  - an entry already owned by a row (`archive_zip_path` + `archive_path_in_zip` match, which is the case for every
    Move and ZIP zip) → known, skip;
  - an entry in `zip_released_entries` → skip;
  - otherwise read the entry's header by decompressing from the entry start until the FITS `END` card (or the XISF
    header length) and stop. Parse with the same header code as disk files and build the frame and the stored header,
    so `header_fingerprint` is computed exactly as for a disk file. Then either adopt an existing row (§7.2) or insert
    a new row with its virtual path.
- The calibrated-light skip rule (`CALSTAT` + `ATH_CSRC`) applies to zip entries too.
- Entries that disappeared from a changed zip: their rows become missing, never deleted.
- Reads go through a large buffer (1–4 MB) so SMB does not see many tiny requests.
- During a scan zip-resident rows get no `content_hash` and are not part of the master full-hash pass (both would read
  whole entries over the network). The content-index job (`duplicates/backfill.rs:152`, which selects every
  `content_hash IS NULL` row and opens its path) skips zip-resident rows. Both `strong_hash` and `content_hash` are
  banked when an entry is fully extracted (§7.4). Until then a zip/disk pair groups only under the default header key,
  not in content-hash mode (designer's call).
- The scan reports zips in its progress (per zip, with its name), like other scan phases.

### 7.2 Moving into a zip, and the hijack fix

The scanner today follows a moved disk file: a new file whose header fingerprint matches an existing row whose path no
longer exists takes over that row (`scanner/mod.rs:502-560`, `1942-2000`). Zip entries get the same rule, mirroring
those conditions exactly (same fingerprint match, same "old path gone" test, same handling of an offline root): the
row becomes zip-resident (`files.path` → virtual path, zip markers and CRC32 set) and keeps everything attached to it.
If the old file still exists, the entry is a new row and a duplicate (§5).

The reverse direction is closed: zip-resident rows are excluded from the move lookup for disk files. Today a manually
unzipped copy of a Move and ZIP frame takes over the archived row while its zip markers stay set. Under §5 a disk copy
of a zip-resident frame is a new row and a duplicate.

### 7.3 The file browser

- Zips with `frame_entries > 0` appear in the dual-pane browser as their own item kind, with the count of cataloged
  frames. Opening one lists its structure from the central directory (read live; it is a few kilobytes). Entries that
  are cataloged show the same metadata as cataloged files do; other entries (text, images) are listed by name and size.
- Inside a zip everything is read-only: no delete, move, rename, F8 or drag of single entries. Preview and the header
  view of a frame entry work through materialize.
- The zip as a whole: move and rename rewrite `archive_zip_path`, the virtual `files.path` of its foreign rows and
  `zip_archives.path` with the same separator-strict prefix swap used for directories (also when a folder holding zips
  is moved or renamed). Black Hole takes the zip as one item.
- Send to Void of a zip deletes the zip file and all its zip-resident rows, as Void of a file deletes that file's row.
  Before it, the confirmation shows how many cataloged frames the zip holds and, for one of our Move and ZIP zips,
  that it is the only copy of an archived frame set.
- "Locate" on a zip-resident row opens the zip in the browser with that entry selected.

### 7.4 Materialize: the one extraction primitive

```rust
/// Local paths for these rows: disk rows as-is, zip-resident rows extracted into the cache.
/// The returned guard pins the cache files until dropped.
pub fn materialize(ctx: &ServiceContext, file_ids: &[i64], progress: &dyn ProgressEmitter,
                   cancel: &CancelFlag) -> Result<Materialized>;
/// The plan-dialog numbers: how many rows are zip-resident and not cached, and their total size.
pub fn estimate_extraction(ctx: &ServiceContext, file_ids: &[i64]) -> Result<ExtractionEstimate>;
```

- Extraction seeks to the entry, streams its compressed bytes through the decompressor into the cache, checks the
  CRC32 and the size, and banks `strong_hash` (`package::xxh3_full_file` format) and `content_hash` (the same sampling
  as `compute_xxhash`) from the extracted bytes.
- Two callers asking for the same entry at once share one extraction.
- A failed entry (zip missing, CRC mismatch, entry gone) fails that frame with a clear reason ("inside M31.zip, which is
  not reachable"); the caller decides, as today, whether one bad frame stops the operation.
- Cancellation stops between and inside entries and leaves no partial cache file.

### 7.5 The cache

- Desktop: the OS cache directory of the app; Docker: under `/data` (overridable by `$ATHENAEUM_ZIP_CACHE`).
- Layout keeps the entry's own file name (`<cache>/<key>/<filename>`): exports and logs that use a file's name see the
  real name. The key is derived from zip path, entry path, zip size + mtime and CRC32.
- Size limit is a setting (Settings → General → Archive, default 32 GB, designer's call). Least recently used unpinned
  files are evicted after an operation releases its pins. An operation that needs more than the limit still runs; the
  limit is restored afterwards.
- "Clear cache" action beside the limit, showing the current size.

### 7.6 Permanent unpack

- Offered on a frame set with zip-resident frames (FrameSetDetail toolbar, beside the existing archive actions) and on
  a calibration set. Target: a folder the user picks; the suggested default is a folder next to the zip named after it.
  Entries keep their path inside the zip under the target.
- Each entry is extracted, verified (CRC32 + size), then the row is updated in one transaction: `files.path` = the new
  file, zip markers cleared, `(zip, entry)` added to `zip_released_entries`. All metadata, analysis and plate-solve
  results stay on the row.
- Released entries are not re-added on a rescan, so a user's own unpack does not double the set right away
  (designer's call; §5's duplicate rule is for copies found by scanning).
- A target file that already exists is never overwritten: if it hashes to the entry it is adopted, otherwise it is a
  conflict and the row stays zip-resident (the same reconcile rule as today's restore).
- The zip is always kept (§5).

### 7.7 Our Archive feature on the new model

- Move and ZIP, rollback and resume are unchanged, with two exceptions:
  - finalize also writes the `zip_archives` row and the entries' CRC32;
  - the planner skips members that are already zip-resident, lights and calibration-chain frames alike, instead of
    failing with "source file no longer exists" (`archive/planner.rs:165, 375`). Only frames on disk are zipped. When
    no member is on disk, Move and ZIP is disabled with the reason "all frames are already in zip files"; Move to
    Archive (the soft hide) still works.
- Zipped sets keep their Archive-page place, badge and Unarchive button. Their frames now work everywhere through
  materialize. The current "restore from archive first" blockers (stacking masters readiness, raw calibration export,
  master rebuild) are replaced by materialize.
- Unarchive keeps its semantics (fill gaps at `source_path`, hash-verify against `expected_hash`, never overwrite,
  conflicts keep the zip, optional zip delete for our own zips only). Its extract stage changes from "every entry of
  every zip into a temp dir" to "only the entries whose target is missing", through the same extraction code. Entries
  restored while the zip is kept go to `zip_released_entries`.
- Unarchive to an alternate folder already exists (`<picked>/<path-in-zip>`); it and the permanent unpack of §7.6 are
  the same code path.

### 7.8 Every consumer of file bytes

Each consumer below calls `materialize` instead of opening `files.path`. Audit list from 2026-10-27 (file:line at that
date; paths under `crates/athenaeum-core/src/` unless named):

| Consumer | Where |
| ---- | ---- |
| Preview, desktop and web | `athenaeum-tauri/src/commands_rustafits.rs:36`, `api/files.rs:322, 386`, `athenaeum-web/src/routes/images.rs:49` |
| Plate solve (single and batch) | `db/operations.rs:3268` callers in `commands/plate_solve.rs:168`, `routes/plate_solve.rs:194` |
| Star analysis, frame metrics, flat contour | `api/analysis.rs:240, 313, 498, 587` |
| Stacking plan probes and run | `stacking/plan.rs:2224-2240, 2322`, `stacking/run.rs:2467` → `prepare.rs:83, 100` |
| Project stacking and project export | `stacking/project_source.rs`, `export/project_organizer.rs` |
| Export WBPP and calibrated lights | `export/file_organizer.rs:668, 676, 833`, `export/calibrated_generator.rs:560`, `calibration_library/light_resolve.rs:72-80, 381-385`, `export/data_collector.rs:545, 612`, `api/lights.rs:563` |
| Master build and rebuild | `api/masters.rs:689-720, 1153`, `api/lights.rs:1014` |
| Duplicate pair verification | `api/files.rs:1297` |
| Collab blink | `api/collab_blink.rs:107` |

Rules for consumers:

- The preview path gets a file id, not a path from the frontend, wherever it does not already.
- WBPP export in link mode copies zip-resident files instead of linking (a link into the cache dangles after
  eviction). The export dialog says so in its "From zip" line.
- Stacking and export plan dialogs, batch plate solve and analysis show the §5 "From zip: N files, X GB" line from
  `estimate_extraction`.
- Project stacking and project export of own frames that are zip-resident go through materialize like any stack or
  export (designer's call: it is data work on the user's own machine, not a send).

### 7.9 What zip-resident rows refuse

- Black Hole and Send to Void of a single zip-resident row: refused with "this frame is inside <zip>; only the whole zip
  can be removed". (Send to Void today deletes the row and orphans the entry.) The whole zip goes through §7.3.
- Relink: zip-resident rows are never reported as orphaned by their `files.path`; a root swap applies to
  `archive_zip_path` and `zip_archives.path` too.
- Transfers, personal sync and collab publish: zip-resident files stay ineligible in v1, with the reason "inside a zip
  — unpack it first" instead of "missing on disk" (designer's call: the wire is frozen; materializing for send is a
  phase-2 design).

### 7.10 Frontend

- A "zip" chip on frame sets in Objects whose frames are all or partly zip-resident ("zip" / "N of M in zip"), and on
  zip-resident rows in frame tables. Design tokens only.
- The zip item and the read-only zip view in the dual-pane browser (§7.3).
- Extraction progress goes through the progress event of the operation that asked for it; `notify()` only on
  failure. The Unpack action has its own progress and an `archive-finished`-style finish event.
- Settings: cache limit and Clear cache in `general.archive` (`src/settings/registry.ts`), following
  `docs/settings/README.md`.

## 8. Commands

New Tauri commands with matching Axum routes (thin wrappers over `athenaeum-core/src/api/zip.rs`):

| Command | Does |
| ---- | ---- |
| `list_zip_entries` | §7.3 structure of one zip, with catalog metadata for cataloged entries |
| `estimate_zip_extraction` | §7.4 estimate for a list of file ids |
| `unpack_zip_frames` | §7.6 for a frame set or calibration set, with a target folder; progress + finish events |
| `get_zip_cache_status` | size, limit, entry count |
| `clear_zip_cache` | removes unpinned cache files |

Existing commands change internally and keep their names: the file-browser listing (zip items), move/rename (§7.3
prefix swap), Black Hole add/list/Void (zip target, single-entry refusal), and every byte reader of §7.8. The command
count in `docs/architecture/commands.md` goes up by five.

## 9. Logging

New `tracing` fields: `zip_path`, `zip_entry`, `bytes_read`, `cache_hit`. They need the logging spec update that new
field names require (`docs/logging/README.md`). The metadata scan logs, per zip, entries cataloged and `bytes_read`, so
the "no whole-zip read" anti-goal can be checked from the logs.

## 10. Left to the builder

- The reader-based header parse (the parser is path-based today: `parse_fits(path, file_id)`); keep one header code
  path for disk and zip.
- The cache index (filesystem metadata or a small table) and the pin mechanism.
- The Black Hole schema shape for a zip target.
- Exact chip wording and placement within the design tokens.
- Whether the lazy CRC32 backfill for existing Move and ZIP rows runs in the migration or on first materialize.

## 11. Designer's calls (for the owner to check)

- Content-hash duplicate mode sees a zip/disk pair only after the entry has been extracted once (§7.1).
- Cache default limit 32 GB; an operation larger than the limit still runs (§7.5).
- A user's own unpack or Unarchive with the zip kept does not re-add the released entries on rescan (§7.6).
- Project stacking and project export use materialize (§7.8).
- Transfers, sync and collab publish of zip-resident files are phase 2 (§7.9).
- Zips with no frames are recorded (to skip them by stat) but not shown in the browser (§6.2, §7.3).

## 12. Phase 0: measurement before the plan

A throwaway probe in core `examples/` (lab tools rule: probes stay in core examples) against a real zip on the owner's
SMB share: central-directory read time, header reads for every frame entry (time and bytes), one full entry extraction
(time, MB/s), for one Stored and one Deflate zip. The numbers go into the plan and set the acceptance bars below.

## 13. Acceptance

- A foreign Deflate zip of a full night (lights + calibration) on the owner's NAS is cataloged with total network reads
  under 1% of the zip's size; its lights form the expected frame set, the calibration matches, the set shows in Objects
  with the zip chip.
- A rescan of the unchanged zip reads nothing but its `stat` (the log shows no `bytes_read` for it).
- A frame present both on disk and in the zip shows as a duplicate pair.
- An already cataloged night, zipped outside the app with the originals deleted: after a rescan the same rows are
  zip-resident, with analysis and plate-solve results intact, and the set's frame count is unchanged.
- The zip opens in the file browser, shows its structure and frame metadata, refuses single-entry delete; moving the
  zip keeps its frames present; Void of the zip deletes it and its rows after the count confirmation.
- Preview, plate solve, analysis, a stack and a WBPP export (copy and link mode) run on the zipped set; the export in
  link mode produces copies; the cache shrinks back to the limit afterwards.
- Permanent unpack moves the rows to disk with metadata intact; a rescan of the kept zip does not re-add them.
- Move and ZIP on a set with half its lights in a foreign zip zips only the disk half.
- A Move and ZIP set: stacking it works without Unarchive; Unarchive after deleting two restored files extracts exactly
  those two entries.
- The NAS share unmounted: zip-resident rows show as missing, nothing is deleted, operations fail per frame with the
  "not reachable" reason; remounting brings them back.
- The same checks pass in the web build (Docker, NAS as a volume).
- `cargo test -p athenaeum-core` (all targets) green; the frontend build green.
