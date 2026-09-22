# Dual-pane file browser

> Moved verbatim out of `CLAUDE.md` on 2026-09-22. This file is the reference for the subsystem; `CLAUDE.md` keeps only the rules, the file map and a pointer here. A cycle that changes this subsystem updates THIS file (acceptance paragraphs, rulings, measurements) and touches `CLAUDE.md` only if a rule or a path in its summary changed.


`FileManager → Browse Files` is a Far-Manager-style two-pane browser that owns file-system operations (Move, Delete, Rename, Mkdir), catalog search, bulk metadata editing, and the Blink launcher. Spec: `docs/superpowers/specs/2026-05-05-dual-pane-file-browser-design.md`.

**Module structure**:

- `crates/athenaeum-core/src/services/operation_queue.rs` — single serialized worker thread shared with the archive feature. `OperationKind { ZipArchive, FileOpMove, FileOpReconcile }` (`FileOpReconcile` is the startup auto-reconcile of abandoned cross-volume commits; it owns no `file_operations` row, so its `operation_id` is always 0).
- `crates/athenaeum-core/src/file_op/` — Move pipeline (`models`, `db`, `planner`, `executor`, `reconcile`). The planner picks `MoveStrategy::AtomicRename` or `MoveStrategy::CopyVerifyDelete` from the source/destination device ids (`MetadataExt::dev()` on unix; volume-root hash on Windows). Same device id ≠ `rename(2)` works — Linux bind mounts and Windows folder-mounted volumes both return `EXDEV`, so an `EXDEV` at **execute** time degrades that one row to `CopyVerifyDelete` instead of failing the batch (`run_cross_volume_fallback`; a resume detects the degradation via the existing `Copy` step). Cross-volume moves verify with xxHash before deleting source. Move planner refuses destination collisions up front. `MoveStrategy::Delete` / `FileOpKind::Delete` still exist as vestigial variants in `models.rs` but are unreachable: the planner never emits them and `executor::run_operation` rejects a `kind='delete'` row loudly.
- `crates/athenaeum-core/src/fits_parser/stored_header.rs` — re-decodes the `fits_header.header` blob into the canonical `FrameOriginalSnapshot` for "what the file looked like at scan time" + per-field revert.
- `src/components/dualpane/` — `DualPaneFileBrowser.tsx`, `MetadataPane.tsx`, `CatalogSearch.tsx`, `types.ts`.

**Key Tauri commands** (mirrored in `crates/athenaeum-web/src/routes/files.rs`):

- File ops: `enqueue_move_operation`, `mkdir_in_scan_root`, `rename_path`. There is no delete / cancel / list-unfinished file-operation command — **user-facing Delete is the Black Hole flow** (`move_to_black_hole` / `bulk_move_to_black_hole` / `send_to_void` in `commands/duplicates.rs`), which is what the dual-pane's F8 calls.
- Search: `search_catalog` (filename / path / OBJECT / FILTER / IMAGETYP / INSTRUME / TELESCOP).
- Metadata pane: `bulk_update_frame_metadata`, `count_frame_metadata_relations`, `get_frame_memberships`, `get_frame_metadata_originals`.

**Hot-sync semantics**:

- **Move**: per-file SQL transaction updates `files.path` AND does the disk action. AtomicRename is `rename(2)`; CopyVerifyDelete is copy → xxHash verify → DB update + source delete. Path-based UPDATE in `update_files_path_by_old_path` is the primary catalog write (id-based update is a fallback). Survives path-spelling variance (macOS `/Volumes` vs `/private/Volumes`, Windows `\\?\` verbatim) **structurally, not by special-casing**: the planner stores and the executor matches the scanner's own non-canonicalized spelling — there is no `canonicalize` on the hot-sync path, and none should be added. A zero-row sync on a catalog-eligible file `warn!`s (it is the spelling-drift signature).
- **Directory rename**: SUBSTR-based leading-prefix swap on `files.path`, bounded by the separator-strict byte range instead of `LIKE` (`db/operations.rs::rename_files_path_prefix`, since `81aedae7`): `UPDATE files SET path = ?new_prefix || SUBSTR(path, LENGTH(?old_prefix) + 1) WHERE path >= ?old_prefix AND (?old_hi IS NULL OR path < ?old_hi)`, with both prefixes ending in a separator and `?old_hi = path_prefix_upper(old_prefix)`. The range is exact-case and literal — unlike `LIKE` it can't cross-match a differently-cased sibling root or one containing `%`/`_`. Naive `REPLACE(path, old, new)` was unsafe — replaced every occurrence, not just the leading one.
- **`bulk_update_frame_metadata` cascade**: deletes `calibration_set_frames`, `calibration_set_to_frames`, `session_members` rows for touched frames; **prunes calibration sets that lose their last member**. FK CASCADE on `calibration_set_to_frames.calibration_set_id` cleans consumer references. Sessions / imaging_nights / frames_set are intentionally left in place even when empty.
- **`bulk_update_calibration_metadata`** (Equipment page) propagates set-level edits to every member frame with `frames.override = 1` so the scanner won't undo it.
- **Override flag**: any save sets `frames.override = 1`; trailing `recompute_override_flag_for_frames` clears it back to 0 if everything matches FITS-header originals (semantic compare: ±1e-6 floats, instant-aware DATE-OBS).

