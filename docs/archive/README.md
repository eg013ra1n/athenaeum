# Archive feature

> Moved verbatim out of `CLAUDE.md` on 2026-09-22. This file is the reference for the subsystem; `CLAUDE.md` keeps only the rules, the file map and a pointer here. A cycle that changes this subsystem updates THIS file (acceptance paragraphs, rulings, measurements) and touches `CLAUDE.md` only if a rule or a path in its summary changed.


Moves a finished frame set's data into a `.zip` per frame type (Lights / Flats / Darks / Bias / DarkFlats) inside a user-configured archive folder, preserving catalog metadata. Full design in `docs/superpowers/specs/2026-04-29-archive-feature-design.md` and plan in `docs/superpowers/plans/2026-04-29-archive-feature.md`.

**Three-state lifecycle for a frame set:**

| State | DB columns | Toolbar button |
| ----- | ---------- | -------------- |
| Stage / WIP | `is_archived = 0` | **Find new images** + **Move to Archive** |
| In Archive section, not zipped | `is_archived = 1`, `archived_at = NULL` | **Move and ZIP** |
| Zipped | `archived_at IS NOT NULL` | **Unarchive** + reveal-in-file-manager |

The legacy `is_archived` boolean is the soft-hide flag (`archive_frame_set`, used by Objects-page tabs; a zipped set's **Unarchive** is the restore operation, which clears `archived_at` and `is_archived` together). The ZIP feature adds `archived_at` + `archive_operation_id` as a separate axis. The planner refuses to ZIP a frame set unless `is_archived = 1` AND `archived_at IS NULL`.

**Module structure (`crates/athenaeum-core/src/archive/`)** — `models`, `db`, `path_layout`, `staging`, `zip_writer` (+ `build_zip_with_progress`), `zip_reader`, `shared_calibration`, `root` (`migrate_legacy_archive_root`, `resolve_archive_root`), `planner` (`build_plan` no DB writes / `commit_plan` writes rows), `executor` (`run_operation` drives stages 2–7 with cooperative cancellation), `rollback` (`rollback_operation` restores sources, deletes partial zips, clears zip markers), `resume` (idempotent step log skips Done), `restore` (reconcile-based: extract + hash-verify; skip if file already on disk at `source_path` else copy).

**Multi-folder destinations** in `archive_roots`. `start_archive_operation` / `plan_archive_operation` accept an optional `archive_root_path`; resolution is explicit > only-root > `is_default` > error. Legacy single-folder `archive.root_path` setting auto-migrates on first read of `list_archive_roots`.

**Tauri commands** (mirrored in `crates/athenaeum-web/src/routes/archive.rs`): folder management (`list_archive_roots`, `add_archive_root`, `delete_archive_root`, `set_default_archive_root`); operation lifecycle (`plan_archive_operation`, `start_archive_operation`, `cancel_archive_operation`, `list_unfinished_archive_operations`, `resume_archive_operation`, `rollback_archive_operation`); browsing (`list_archived_frame_sets`, `list_archive_zips`); restore (`start_restore_operation`, `get_restore_suggestions`); cleanup (`delete_archive`).

**Progress events**: unified on `archive-progress` for both archive and restore stages; `archive-finished` fires at exit with `{ operation_id, outcome, kind? }` so the widget auto-dismisses with the right color.

**Zip build and rollback model**: `zip_writer` builds a zip into `<zip>.<pid>-<n>.partial` beside the target, fsyncs it, the executor verifies the partial's entry list and only then renames it onto the target (an overwritten zip is untouched until that rename; any failure removes the partial). Right after the rename the executor records a `zip_publish` step. `rollback_operation` removes the operation's stale partials (dead or own pid, exact name pattern) and a target zip only when a `zip_publish` step exists for it; it restores a source through a sibling temp (write, fsync, rename) and trusts an existing source only when it hashes to the row's `expected_hash` (the sampled hash), so a truncated leftover is re-copied; it refuses (before touching anything, status unchanged) while a Move source is neither intact on disk nor in staging, since the zip may then be the only copy; and it refuses `completed` and `rolled_back` operations.

**Restore semantics (the safe one)**: zip is the inventory; restore makes disk match by filling gaps. For each `archive_operation_files` row, if the file already exists at `source_path` skip (no overwrite, no duplicate); else copy from temp → target. Cleanly handles copy-disposition calibrations and cross-archive-move cases.


## Detail moved from CLAUDE.md (2026-10-20) - Archive rules

### Archive feature

Moves a finished frame set's data into one `.zip` per frame type under a configured archive folder, keeping catalog metadata. Three states on `frames_set`: WIP (`is_archived = 0`) → soft-archived (`is_archived = 1`, `archived_at NULL`, toolbar **Move and ZIP**) → zipped (`archived_at` set, **Unarchive**); the planner refuses to ZIP unless `is_archived = 1 AND archived_at IS NULL`. Modules in `crates/athenaeum-core/src/archive/`: `planner` (`build_plan` no DB writes / `commit_plan` writes rows), `executor` (cooperative cancel), `rollback`, `resume` (idempotent step log), `restore` (reconcile-based: the zip is the inventory, fill gaps, never overwrite), `zip_writer`/`zip_reader`, `path_layout`, `shared_calibration`. Multi-folder destinations in `archive_roots` (explicit > only-root > `is_default` > error). Events `archive-progress` + `archive-finished`; commands mirrored in `routes/archive.rs`. Spec `docs/superpowers/specs/2026-04-29-archive-feature-design.md`; reference `docs/archive/README.md`.

