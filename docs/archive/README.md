# Archive feature

> Moved verbatim out of `CLAUDE.md` on 2026-09-22. This file is the reference for the subsystem; `CLAUDE.md` keeps only the rules, the file map and a pointer here. A cycle that changes this subsystem updates THIS file (acceptance paragraphs, rulings, measurements) and touches `CLAUDE.md` only if a rule or a path in its summary changed.


Moves a finished frame set's data into a `.zip` per frame type (Lights / Flats / Darks / Bias / DarkFlats) inside a user-configured archive folder, preserving catalog metadata. Full design in `docs/superpowers/specs/2026-04-29-archive-feature-design.md` and plan in `docs/superpowers/plans/2026-04-29-archive-feature.md`.

**Three-state lifecycle for a frame set:**

| State | DB columns | Header button |
| ----- | ---------- | -------------- |
| Stage / WIP | `is_archived = 0` | **Find new images** (**Archive**, the soft Move to Archive, is at the end of the Lights toolbar with **Unpack** and **Duplicates**) |
| In Archive section, not zipped | `is_archived = 1`, `archived_at = NULL` | **Move and ZIP** |
| Zipped | `archived_at IS NOT NULL` | **Unarchive** + reveal-in-file-manager |

The legacy `is_archived` boolean is the soft-hide flag (`archive_frame_set`, used by Objects-page tabs; a zipped set's **Unarchive** is the restore operation, which clears `archived_at` and `is_archived` together). The ZIP feature adds `archived_at` + `archive_operation_id` as a separate axis. The planner refuses to ZIP a frame set unless `is_archived = 1` AND `archived_at IS NULL`.

**Module structure (`crates/athenaeum-core/src/archive/`)** — `models`, `db`, `path_layout`, `staging`, `zip_writer` (+ `build_zip_with_progress`), `zip_reader`, `shared_calibration`, `root` (`migrate_legacy_archive_root`, `resolve_archive_root`), `planner` (`build_plan` no DB writes / `commit_plan` writes rows), `executor` (`run_operation` drives stages 2–7 with cooperative cancellation), `rollback` (`rollback_operation` restores sources, deletes partial zips, clears zip markers), `resume` (idempotent step log skips Done), `restore` (reconcile-based: extract + hash-verify; skip if file already on disk at `source_path` else extract that one entry), `zip_store` (outside `archive/`: the reader, cache and `materialize` every zip consumer uses).

**Multi-folder destinations** in `archive_roots`. `start_archive_operation` / `plan_archive_operation` accept an optional `archive_root_path`; resolution is explicit > only-root > `is_default` > error. Legacy single-folder `archive.root_path` setting auto-migrates on first read of `list_archive_roots`.

**Tauri commands** (mirrored in `crates/athenaeum-web/src/routes/archive.rs`): folder management (`list_archive_roots`, `add_archive_root`, `delete_archive_root`, `set_default_archive_root`); operation lifecycle (`plan_archive_operation`, `start_archive_operation`, `cancel_archive_operation`, `list_unfinished_archive_operations`, `resume_archive_operation`, `rollback_archive_operation`); browsing (`list_archived_frame_sets`, `list_archive_zips`); restore (`start_restore_operation`, `get_restore_suggestions`); cleanup (`delete_archive`).

**Progress events**: unified on `archive-progress` for both archive and restore stages; `archive-finished` fires at exit with `{ operation_id, outcome, kind? }` so the widget auto-dismisses with the right color.

**Zip build and rollback model**: `zip_writer` builds a zip into `<zip>.<pid>-<n>.partial` beside the target, fsyncs it, the executor verifies the partial's entry list and only then renames it onto the target (an overwritten zip is untouched until that rename; any failure removes the partial). Right after the rename the executor records a `zip_publish` step. `rollback_operation` removes the operation's stale partials (dead or own pid, exact name pattern) and a target zip only when a `zip_publish` step exists for it; it restores a source through a sibling temp (write, fsync, rename) and trusts an existing source only when it hashes to the row's `expected_hash` (the sampled hash), so a truncated leftover is re-copied; it refuses (before touching anything, status unchanged) while a Move source is neither intact on disk nor in staging, since the zip may then be the only copy; and it refuses `completed` and `rolled_back` operations.

**Restore semantics (the safe one)**: zip is the inventory; restore makes disk match by filling gaps. For each `archive_operation_files` row, if the file already exists at `source_path` skip (no overwrite, no duplicate); else extract that one entry straight to its target (no temp tree; `zip_store::place::place_entry`, which never replaces a file). Cleanly handles copy-disposition calibrations and cross-archive-move cases. Detail of the zip model and of a partial Unarchive: **Zip archives in the catalog** below.


## Zip archives in the catalog (zip plan, 2026-10-28)

Spec `docs/superpowers/specs/2026-10-27-zip-archives-in-catalog-design.md`, plan `docs/superpowers/plans/2026-10-27-zip-archives-in-catalog.md`. Scanner-side rules (foreign zips, virtual paths, presence, cache, hashes): `docs/catalog/README.md` -> Zips in the catalog. Black Hole and browser rules: `docs/file-browser/README.md`.

**Acceptance (NAS, desktop, owner: works, 2026-10-31).** The owner ran zips on the NAS share in the desktop build on the real dev DB; what turned up was fixed on `main` (the zip-entry rows and the Send to Void rescan, merged in bc9535df).

**Acceptance (web build, Docker on the Unraid NAS, 2026-10-31).** An amd64 image of `main` a5d5081c, its own container
beside the owner's, the share's test folder bind-mounted at `/astro-files`, driven through the HTTP API with Settings seen
in the browser. Test data: 10 IC 5146 B lights, 15 flats and 12 darks (QHY268M) as plain files, `GAIA_smoke.zip` (12
lights), and 3 of its lights also as plain files.
- **Scan.** It found 40 plain files plus 12 zip entries, with 0 errors. The GAIA set counts 12 frames: 9 from the zip and 3
  duplicates, each counted once (180 s = 12 × 15 s). The listing shows the zip's 12 entries.
- **Preview.** A zip frame previews (a 1 546 119-byte JPEG, the same size as the Windows run's). The cache lands in
  `/data/zip-cache/<key>/<entry name>`.
- **Settings → General → Archive.** The page shows the limit, the status ("49.8 MB in 1 unpacked frame") and Clear cache.
  A changed limit shows at once in the status, and Clear cache empties it.
- **`ATHENAEUM_ZIP_CACHE`.** The variable moves the cache to `/astro-files/zip-cache-alt/<key>/…`. The old cache is left
  where it was, and the status no longer counts it.
- **Plate solve from the zip.** It needed the Gaia catalog first (Settings download, 8 min 45 s). After that, 3 of 3
  frames solved in 1.8 s, with 179–234 stars matched and rms 0.53–0.55″. The solved scale is 2.877″/px, against 2.883″/px
  from the header.
- **Analysis.** 12 of 12 frames analysed. For each duplicated frame it used the disk copy.
- **Move and ZIP of IC 5146.** Lights and flats were moved in, darks skipped. It wrote two zips (522 MB and 783 MB), and the
  set reads 10 of 10 frames from the zip. The master flat was built from the zipped flats. The stack ran from the zips in
  40 s, and the master is clean (no dust rings, no vignetting).
- **Export.** In copy mode, 37 files were placed; a zipped light is byte-identical to its original (md5). In link mode, the
  25 zip frames were copied (`zipCopied` 25) and the 12 disk darks were linked.
- **Unpack.** 2 GAIA frames were unpacked. Their entries show `released` with the copy path, and a rescan skips the
  unchanged zip.
- **Whole-zip actions.**
  - Black Hole on the zip: the GAIA set drops to 5 frames, and export readiness to 5. Restoring the zip brings back 12
    frames, 7 of them in the zip.
  - Send to Void deletes the zip and its 10 remaining rows; the duplicates list becomes empty.
- **An offline share.** With the scan root renamed away, a scan finds 0 files and deletes nothing, and a zip frame's
  preview returns 404. With the root back, every row is intact and the preview works.

The server logged only expected errors: 404 while the zip was away, 412 before the catalog download, and the r2d2 "database
is locked" line at startup.

**Acceptance (Windows, OBS-PC1, 2026-10-31).** The run used `main` 549e20a5; its report is
`\\Universe\athenaeum-runner\handoffs\zip-windows-main-report.md`.
- **Test suites.** The whole core suite passes except the 5 `hips::pack` tests, which fail for a test-side reason (open
  items); `athenaeum-web` passes 57 of 57.
- **Windows twins of the `#[cfg(unix)]` tests.** These ran in the runner's clone; the patch is beside the report. All of
  them pass:
  - Overwrite refuses a zip whose rows name it through a junction or in another letter case (`file_identity`).
  - Unpack through a junction that points outside the destination refuses every entry and writes nothing.
  - A poisoned cache key is never a hit while another handle holds its file.
- **The release web server on the UNC share.** It stores every path with `\`. The scan, duplicates, listing, preview and
  cache all match the Docker run. On a local copy, Unpack and Move and ZIP pass.
- **One finding.** Publishing a zip over a target with the read-only attribute fails with "Access is denied"; the target is
  left untouched. It is listed in open items.

**Accepted for zips, do not re-flag** (moved here from open items, 2026-10-31):
- The cache can pass its limit by one group during a stacking Prepare; the limit is a target.
- A project re-export of a zip frame always extracts it again. A correct skip would need the entry CRC in the export record.
- Unpack has no Cancel.
- A set whose foreign-zip frames are all calibration frames gets no "Unpack from zip" button; reading them still works.
- `RawWithCalibrationSets` still places an imported master that sits in a Black Hole zip.
- Rollback restores over a non-matching file at the original path.
- Project stacking of zipped landed files is unreachable (`landed_unchanged`).
- **A different frame written over the unpacked path keeps the release alive** (review r1, 2026-10-29). The disk walker
  re-parses the row in place and the link still names a file that exists, so the zip entry stays out of the catalog and
  "unpacked -> path" names another frame. Telling them apart needs a second fingerprint column or a `linked` marker.
  Locate names the row's current path, so nothing is hidden. Fix it when a real report comes.
- **A zip view under an unmounted copy root shows the released entry as readable, without the "unpacked" chip** (review
  r1). `released_entries_of_zip` keeps a release only when a candidate path exists, while the scan keeps it under an
  offline root (`present_or_offline`). Nothing is written, and the chip comes back with the mount; cosmetic.

**Rules that bite**

- **Zips are read-only.** No code writes into a zip. Only an explicit Send to Void of the WHOLE zip deletes one; Unarchive deletes a zip only if it is one of OUR zips, only on request, and only when no catalog row still lives in it.
- **One kind of zip-resident row.** A `files` row with `archive_zip_path` + `archive_path_in_zip` has no file at `files.path`. Our Move and ZIP rows keep `files.path` = the original disk location (the restore target) and `archived_in_operation`; a foreign row has `archived_in_operation IS NULL` and the virtual path `<zip>/<entry>`. Every byte reader goes through `zip_store::materialize`, never `files.path`.
- **Two hashes, never compared.** `archive_operation_files.expected_hash` is the SAMPLED `duplicates::compute_xxhash` (3 x 512 KiB); `files.strong_hash` and an extraction's `strong_hash` are the full-file `package::xxh3_full_file`. Comparing one with the other fails every time. A test that checks a hash against `expected_hash` needs a fixture file over 1.5 MiB or the sample covers the whole file and hides the mistake.
- **Missing never deletes.** A zip on an unmounted share makes its rows missing (`zip_store::presence::row_is_present`); no archive path deletes a row for a missing zip, and Void of a zip refuses when the zip is not reachable.
- **Black Hole zips are not Unarchived.** A zip in the Black Hole (owner ruling A) is refused by Unarchive and by unpack ("unarchive refused: zip is in the Black Hole" warn) until it is restored; restore's `forget_zip` cascade never touches a Black Hole row of a zip it did not just delete.

**The model.** `zip_archives` (`path` UNIQUE, `size`, `modified_at`, `frame_entries`, `scanned_at`, `created_by_operation`) has one row per zip the scanner read or an operation built; `zip_released_entries (zip_id, entry_path)` lists entries whose row was moved to disk by an unpack or by an Unarchive that kept the zip, so a rescan does not re-add them. `files.archive_entry_crc32` is the entry's CRC32 (extraction verifies it; the cache key includes it). Finalize writes the `zip_archives` row (`record_built_zip`: the real stat in the format the zip scan writes, so the scan finds it equal, plus the entry count and `created_by_operation`; it also clears stale released entries for that path) and every moved row's CRC32 (`set_file_entry_crc32`). Rows of zips built before the cycle are backfilled by the migration (`size = 0, modified_at = ''` when the zip was not on disk: that sentinel means "read me again", and no listing may show it as a real value) and their CRC32 is filled lazily, the first extraction verifying them by `expected_hash`; until then they are extracted without the catalog CRC check and bank no hash. `unmark_file_archived` clears the CRC with the other archive markers.

**Planner skip.** `build_plan` leaves out a member that is already zip-resident (`is_zip_resident`), lights and the calibration chain alike, instead of failing on "source file no longer exists"; only frames on disk are zipped, and frames already in a zip count as archived (log `frames already in zip files left out of the archive plan`). Owner-visible rulings:

- When no member is on disk the plan is refused with **"all frames are already in zip files"**; Move to Archive (the soft hide) still works.
- A frame-set Move and ZIP bails with the same text when NO LIGHT is on disk, even if its calibration frames are (they would make a calibration-only zip and mark the set zipped); a set with no lights anywhere says "nothing to archive - the set has no lights on disk". An empty plan is never committed.
- A target zip that catalog rows still live in is refused ("already holds N cataloged frame(s); it cannot be replaced. Choose a different archive folder or unpack those frames first"): the archive would truncate it.

**Build beside, then rename.** `zip_writer` builds into `<zip>.<pid>-<n>.partial` beside the target, fsyncs it, the executor verifies the partial's entry list and only then renames it onto the target; an overwritten zip is untouched until that rename and any failure removes the partial. A `zip_publish` step is recorded right after the rename. (This is the model the earlier "Zip build and rollback model" paragraph states; the rollback rules below depend on it.)

**Rollback rules (r4-r6).**

- Deletes only a zip it finished: a target zip is removed only when a `zip_publish` step says this operation put it there; stale `.partial` files of the operation (own or dead pid, exact name pattern) are swept (`swept` in the log).
- **Pre-flight, before anything is touched.** For every Move source the rollback would restore, the source must be intact on disk (hashes to the row's SAMPLED `expected_hash`) or restorable from staging; if any is neither, the zip may hold the only copy and the rollback is refused with nothing changed and the status left as it was (`rollback refused: ...` error with `lost` and `first`). The pre-flight hashes every file of the restore set.
- **Restores through a temp.** A source is rebuilt through a sibling temp (write, fsync, rename) and an existing file at the source path is trusted only if it hashes to `expected_hash`, so a truncated leftover is re-copied. Rollback puts the ARCHIVED bytes back at the original path. Accepted: it also restores over a NON-matching file at that path (a truncated leftover, or, rarely, a new capture placed there after the delete); a killed restore can leave `<source>.<pid>-<n>.partial`, which the scanner ignores (non-frame extension).
- **Completed and RolledBack operations are refused.** A refused rollback leaves the operation resumable (it stays in the unfinished list; `resume` still offers it).

**Partial Unarchive.** Restore extracts only the entries whose target is missing, through `place_entry`; an entry already on disk is hash-verified against `expected_hash` and skipped, and a mismatch is a conflict (file untouched, markers not cleared, loop continues, the zips of an operation with conflicts are kept). Entries restored while the zip is kept are recorded in `zip_released_entries`. Cleanup (`keep_zip_after_restore = false`) deletes a zip only if, after the operation's own markers are cleared, no `files` row still has `archive_zip_path` = that zip (a hand-added entry cataloged as a foreign row, or an adopted row, keeps it: `keeping the zip after restore: catalog rows still live in it`); when it does delete one, `forget_zip` drops its `zip_archives` row and releases. Unarchive to an alternate folder uses `<picked>/<path-in-zip>` and shares `place_entry` with the permanent unpack (`unpack_zip_frames`, foreign rows only: `docs/catalog/README.md`).

**Owner-visible rules (T4 r3).**

- A new capture that takes the original path of one of OUR zipped rows does not collide: the zipped row is re-pointed to its virtual path (`<zip>/<entry>`), freeing the path; the re-point is skipped, with a warn, if another row already owns the virtual path, and the row then follows its zip when the zip is renamed.
- Unarchive to an alternate folder ignores a non-matching file at `source_path` (a newer capture that reused the name is not "already restored" and is not in the way); only the frame's own bytes there count.
- A different file at the original path of one of our zipped rows (not re-pointed) is re-parsed and warned about on every scan, not once.

**Frames work without Unarchive.** Preview, plate solve, analysis, stacking, export, master build and rebuild read a Move and ZIP set through `materialize`; the old "restore from archive first" blockers fire only when the bytes are reachable nowhere (`docs/export/README.md`, `docs/masters/README.md`, `docs/stacking/README.md`). Frame-set Unarchive keeps its place, badge and button on the Archive page.

**Not in v1.** Other containers (7z, RAR, tar), split or encrypted zips, zips inside zips (skipped with a log line); writing into any zip; transfers of zip-resident files (`docs/transfers/README.md`). Candidate follow-ups: a cancel path for unpack (both hosts pass a fresh cancel flag; the dialog says it cannot be cancelled), and an unpack button for a set whose foreign-zip frames are only calibration frames (`UnpackTarget::CalibrationSet` has no UI; reads still work from the zip).

## Detail moved from CLAUDE.md (2026-10-20) - Archive rules

### Archive feature

Moves a finished frame set's data into one `.zip` per frame type under a configured archive folder, keeping catalog metadata. Three states on `frames_set`: WIP (`is_archived = 0`) → soft-archived (`is_archived = 1`, `archived_at NULL`, toolbar **Move and ZIP**) → zipped (`archived_at` set, **Unarchive**); the planner refuses to ZIP unless `is_archived = 1 AND archived_at IS NULL`. Modules in `crates/athenaeum-core/src/archive/`: `planner` (`build_plan` no DB writes / `commit_plan` writes rows), `executor` (cooperative cancel), `rollback`, `resume` (idempotent step log), `restore` (reconcile-based: the zip is the inventory, fill gaps, never overwrite), `zip_writer`/`zip_reader`, `path_layout`, `shared_calibration`. Multi-folder destinations in `archive_roots` (explicit > only-root > `is_default` > error). Events `archive-progress` + `archive-finished`; commands mirrored in `routes/archive.rs`. Spec `docs/superpowers/specs/2026-04-29-archive-feature-design.md`; reference `docs/archive/README.md`.

