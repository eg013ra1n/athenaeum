# Archive feature

> Moved verbatim out of `CLAUDE.md` on 2026-09-22. This file is the reference for the subsystem; `CLAUDE.md` keeps only the rules, the file map and a pointer here. A cycle that changes this subsystem updates THIS file (acceptance paragraphs, rulings, measurements) and touches `CLAUDE.md` only if a rule or a path in its summary changed.


Moves a finished frame set's data into a `.zip` per frame type (Lights / Flats / Darks / Bias / DarkFlats) inside a user-configured archive folder, preserving catalog metadata. Full design in `docs/superpowers/specs/2026-04-29-archive-feature-design.md` and plan in `docs/superpowers/plans/2026-04-29-archive-feature.md`.

**Three-state lifecycle for a frame set:**

| State | DB columns | Toolbar button |
| ----- | ---------- | -------------- |
| Stage / WIP | `is_archived = 0` | **Find new images** + **Move to Archive** |
| In Archive section, not zipped | `is_archived = 1`, `archived_at = NULL` | **Move and ZIP** |
| Zipped | `archived_at IS NOT NULL` | **Unarchive** + reveal-in-file-manager |

The legacy `is_archived` boolean is the soft-hide flag (`archive_frame_set` / `unarchive_frame_set`, used by Objects-page tabs). The ZIP feature adds `archived_at` + `archive_operation_id` as a separate axis. The planner refuses to ZIP a frame set unless `is_archived = 1` AND `archived_at IS NULL`.

**Module structure (`crates/athenaeum-core/src/archive/`)** — `models`, `db`, `path_layout`, `staging`, `zip_writer` (+ `build_zip_with_progress`), `zip_reader`, `shared_calibration`, `planner` (`build_plan` no DB writes / `commit_plan` writes rows), `executor` (`run_operation` drives stages 2–7 with cooperative cancellation), `rollback` (`rollback_operation` restores sources, deletes partial zips, clears zip markers), `resume` (idempotent step log skips Done), `restore` (reconcile-based: extract + hash-verify; skip if file already on disk at `source_path` else copy).

**Multi-folder destinations** in `archive_roots`. `start_archive_operation` / `plan_archive_operation` accept an optional `archive_root_path`; resolution is explicit > only-root > `is_default` > error. Legacy single-folder `archive.root_path` setting auto-migrates on first read of `list_archive_roots`.

**Tauri commands** (mirrored in `crates/athenaeum-web/src/routes/archive.rs`): folder management (`list_archive_roots`, `add_archive_root`, `delete_archive_root`, `set_default_archive_root`); operation lifecycle (`plan_archive_operation`, `start_archive_operation`, `cancel_archive_operation`, `list_unfinished_archive_operations`, `resume_archive_operation`, `rollback_archive_operation`); browsing (`list_archived_frame_sets`, `list_archive_zips`); restore (`start_restore_operation`, `get_restore_suggestions`); cleanup (`delete_archive`).

**Progress events**: unified on `archive-progress` for both archive and restore stages; `archive-finished` fires at exit with `{ operation_id, outcome, kind? }` so the widget auto-dismisses with the right color.

**Restore semantics (the safe one)**: zip is the inventory; restore makes disk match by filling gaps. For each `archive_operation_files` row, if the file already exists at `source_path` skip (no overwrite, no duplicate); else copy from temp → target. Cleanly handles copy-disposition calibrations and cross-archive-move cases.

