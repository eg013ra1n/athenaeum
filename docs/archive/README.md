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

