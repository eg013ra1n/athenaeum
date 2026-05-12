# Dual-Pane File Browser — Design & Implementation

## Context

The original `FileManager → Browse Files` tab rendered a single-pane
`DirectoryTree` with hover-only metadata and no on-disk file operations.
Astrophotographers regularly need to (a) inspect FITS / XISF metadata next to
the file list, (b) reorganize captures across drives and scan roots,
(c) clean up wrong metadata (OBJECT, FILTER, IMAGETYP, etc.), and (d) find
specific files quickly across thousands of frames. This feature replaces the
single-pane browser with a Far-Manager-style **dual-pane** browser that
covers all four use cases without the user leaving the app and without the
catalog desyncing.

The full original implementation plan lived in `~/.claude/plans/`; this
document is the consolidated design + reference for the shipped feature.

## Locked Design Decisions

| # | Decision |
|---|----------|
| Catalog sync | Hot-sync per file. Same DB transaction updates `files.path` + does the disk action. Survives interrupted transfers. |
| Queue arch | Shared executor primitives extracted from archive feature; per-kind tables (`archive_*` stays, new `file_operation_*`); single global worker serializes archive + file ops. |
| Source/Dest | Inside scan roots only (both ends). Mixed file + directory selection. Cross-volume moves use copy + xxHash verify + delete; same-volume use atomic `rename(2)`. |
| File ops | **Move** (queued), **Permanent delete** (queued, simple confirm), **Rename** (sync), **Mkdir** (sync). No copy. |
| Search | Catalog DB search across all scan roots over filename + path + key metadata. Click result → active pane reveals + selects. |
| Metadata pane | Either pane toggles to metadata mode; shows the *other* pane's selection; bulk-edit form (varies-aware). |
| Edit fields | OBJECT, FILTER, IMAGETYP (10-option dropdown including Master* variants), CAMERA / INSTRUME, TELESCOP, FOCALLEN, GAIN, OFFSET, BINNING (1×1–6×6 dropdown), EXPTIME, CCD-TEMP, DATE-OBS. IS-MASTER is auto-derived from IMAGETYP. |
| Input model | Hybrid macOS + Far-style F-keys: F2 Rename / F6 Move / F7 Mkdir / F8 Delete; ⌘A select-all; Shift+click range; Tab switch pane; ⌘I metadata; Space Blink; ↑↓/Enter/Backspace navigate. |
| Web parity | Full: every Tauri command has a matching Axum route; the dual-pane React component works in both Tauri and web modes via `src/api/`. |
| Placement | Replaces `<DirectoryTree>` inside the existing **Browse Files** tab. Other tabs untouched. |

## Architecture

### Backend (Rust)

**`services::operation_queue`** — single serialized worker thread shared by archive and file ops. `OperationKind { ZipArchive, FileOpMove, FileOpDelete }`. Panic-safe (`catch_unwind` around each job). `enqueue(QueuedJob { kind, operation_id, run })` is the only API; archive command sites and file-op command sites both push through it. Tests: `jobs_run_serially_in_order`, `worker_survives_panicking_job`.

**`file_op/`** — Move/Delete pipeline modeled on the existing `archive/` module:

- `models.rs` — `FileOpKind`, `FileOpStatus`, `MoveStrategy { AtomicRename, CopyVerifyDelete, Delete }`, `FileDisposition`, `FileOpStage`, `StepStatus`, plus `FileOperation`, `FileOperationFile`, `FileOperationStep`, `FileOpPlan`, `FileOpProgress`.
- `db.rs` — CRUD for `file_operations` / `file_operation_files` / `file_operation_steps` tables; `update_files_path` and `update_files_path_by_old_path` (the latter is the path-based fallback that catches catalog rows even when the planner missed the `catalog_file_id` lookup due to path-encoding mismatch).
- `planner.rs` — `build_move_plan`, `build_delete_plan`. Validates scan-root containment for source + dest; expands directory selections recursively (Move preserves `dir_basename + relative path`; Delete records every directory walked through, deepest-first). For Move it pre-hashes sources (`duplicates::compute_xxhash`) for cross-volume verify; **fails the entire plan** if any per-file destination already exists (no silent overwrite).
- `executor.rs` — `run_operation`. Per-file step log with idempotent skip-if-Done for resume. Move: AtomicRename (`fs::rename` inside DB transaction with hot-sync; resume-aware via src/dest existence checks) or CopyVerifyDelete (Copy → Verify → CommitMove with foreign-file detection so a fresh op can't overwrite an unrelated file with a matching name). Delete: per-row transaction (DB row removed, on-disk `fs::remove_file`); empty subdirectories removed deepest-first.

**`db::operations` extensions** for the metadata-pane workflow:

- `bulk_update_frame_metadata` — extended to handle OBJECT, FILTER, TELESCOP, FOCALLEN, GAIN, OFFSET, BINNING, EXPTIME, CCD-TEMP. **Auto-derives `is_master`** from the IMAGETYP enum (Master* → 1, base → 0). Cascades all relations (`calibration_set_frames`, `calibration_set_to_frames`, `session_members`) on every save (warn-before-save in UI). **Prunes calibration sets that lose their last member** (master sets and last-frame regular sets) — consumer references via `calibration_set_to_frames` cascade-delete with the set thanks to the FK CASCADE. Sessions / imaging_nights / frames_set are intentionally left in place when empty. Calls `recompute_override_flag_for_frames` at the end so reverting all edits drops the `frames.override` flag back to 0.
- `count_frame_metadata_relations` — pre-save count of cascade impact; drives the unlink-warning dialog.
- `get_frame_memberships_summary` — aggregates which frame_sets / calibration sets the selection is part of and which calibration sets it consumes; also resolves `used_in_frame_sets` (the Objects whose LIGHTs use a calibration the selection is a member of).
- `get_frame_metadata_originals` — joins `frames → files → fits_header`, parses the stored header (FITS card text or XISF XML — see `fits_parser/stored_header.rs`), and returns the canonical "what the file looked like at scan time" snapshot for per-field revert.
- `recompute_override_flag_for_frames` — compares each overridden frame's current values to its FITS-header originals (semantic equality: `±1e-6` for f64s, instant-aware for DATE-OBS) and clears `frames.override = 0` when everything matches.
- `rename_files_path_prefix` — SUBSTR-based leading-prefix swap for directory renames. The naive `REPLACE(path, old, new)` was unsafe because it substituted every occurrence of the prefix in the path.

**`fits_parser/stored_header.rs`** — parses the stored `fits_header.header` blob without re-reading the file from disk. FITS path is line-by-line 80-char card parsing; XISF path is `quick_xml` extraction of `<FITSKeyword/>` elements. `snapshot_from_keys` projects onto `FrameOriginalSnapshot` with normalisation (IMAGETYP → PascalCase via `ImageType::from_str`; XBINNING/YBINNING → `"AxB"`).

**`bulk_update_calibration_metadata`** (Tauri + web) — when an Equipment-page edit writes to a calibration set, the change is now also propagated to every member frame in `calibration_set_frames`, with `frames.override = 1` so the scanner won't undo it. Mirrors the same fields the set update writes (ccd_temp, gain, offset, binning, exptime) and keeps `xbinning/ybinning` in lockstep with the parsed `binning` string.

### Database schema

New tables in `db/schema.rs::init_db` (idempotent `CREATE TABLE IF NOT EXISTS`):

```sql
file_operations         (id, kind, status, source_root, dest_dir,
                         total_files, total_bytes, created_at, started_at,
                         finished_at, error_message)
file_operation_files    (id, operation_id, source_path, dest_path, strategy,
                         catalog_file_id, expected_hash, file_size_bytes,
                         disposition)
file_operation_steps    (id, operation_id, operation_file_id, stage, status,
                         actual_hash, error_message, started_at, completed_at)
```

Indexes: `idx_file_op_files_op`, `idx_file_op_steps_op_file`,
`idx_file_op_steps_status`, `idx_file_ops_status`.

### Tauri / web command surface

| Command (Tauri + web) | Purpose |
|---|---|
| `enqueue_move_operation(sources, destDir)` | Plan + enqueue Move op; returns op_id. |
| `enqueue_delete_operation(targets)` | Plan + enqueue Delete op (after UI confirm). |
| `cancel_file_operation(operationId)` | Set the cancel flag on a queued/running op. |
| `list_unfinished_file_operations()` | Resume on app start. |
| `mkdir_in_scan_root(path)` | Synchronous — validates scan-root containment, creates dir. |
| `rename_path(oldPath, newName)` | Same-folder rename, hot-syncs catalog (file → path/filename; dir → SUBSTR prefix swap on every descendant). |
| `search_catalog(query, limit?)` | DB search across all scan roots over filename / path / OBJECT / FILTER / IMAGETYP / INSTRUME / TELESCOP. Returns hits with full path. |
| `bulk_update_frame_metadata(frameIds, edits)` | DB-only metadata write; cascades; prunes empty cal sets; recomputes override. |
| `count_frame_metadata_relations(frameIds)` | Pre-save unlink-impact count. |
| `get_frame_memberships(frameIds)` | Frame-set + calibration-set memberships + "used in frame set" reverse lookup. |
| `get_frame_metadata_originals(frameIds)` | Canonical original-from-FITS-header snapshot for revert. |

Progress + finished events: `file-op-progress` (mirrors `ArchiveProgress` shape) and `file-op-finished` (`{ operation_id, outcome, kind }`) on both Tauri events and SSE.

### Frontend (React/TypeScript)

`src/components/dualpane/`:

```
DualPaneFileBrowser.tsx   # Top-level shell — reducer-state, two PaneViews,
                          # search bar, status bar, all keyboard handling,
                          # Move/Delete/Mkdir/Rename modals, Blink overlay.
PaneView (inline)         # Per-pane: toolbar (Up / Refresh / scan-root jump /
                          # breadcrumb / metadata-mode toggle), filter+sort
                          # row, body (file list OR metadata pane).
FileList (inline)         # File / folder rows with selection + arrow-key nav,
                          # parent-row (..) with manual double-click detection.
CatalogSearch.tsx         # Debounced catalog search with reveal-on-click.
MetadataPane.tsx          # Bulk-edit form (auto-enable on type, dropdowns
                          # for IMAGETYP and BINNING), MembershipsPanel,
                          # OriginalsPanel with per-field ↺ revert button.
types.ts                  # Path helpers + scan-root clamp + format helpers.
```

State model uses a single reducer in `DualPaneFileBrowser`; per-pane components are presentational. Per-pane `loadGenRef` generation counter guards against out-of-order async resolution (was causing breadcrumb-vs-listing desync after Move).

### Frontend file-row visual cues

- Pencil icon on rows where `frame.override_ === true` ("Custom metadata applied — open metadata pane to compare with original").
- Active pane: amber border + soft amber glow (replaces the older accent-coloured ring).
- `select-none` on the pane shell prevents accidental text-selection on double-clicks; inputs/textareas keep their default selectability via user-agent styles.

## Behavioural notes worth remembering

- **Browsing is clamped to the scan root.** Up button + breadcrumb stop at the enclosing scan root; deeper navigation is unrestricted.
- **Folders behave like files**: single click selects, double click enters. Drag-and-drop is intentionally NOT supported — F6 is the canonical Move path.
- **Filter is ephemeral** (cleared on cwd change). Sort is per-pane and persistent in localStorage (`dualpane.sortDir.{left,right}`).
- **Catalog hot-sync survives path-encoding mismatch**: `sync_catalog_path` always tries the path-based UPDATE first; id-based fallback only fires if the path-based update missed but a known `catalog_file_id` is available.
- **Move plan refuses destination collisions up front** — no silent overwrite on either move strategy. User resolves conflicts manually and retries.
- **Cross-pane recovery**: when a delete on one pane invalidates the other pane's cwd, the loader catches "missing"-class errors, walks up to the nearest existing ancestor (clamped to scan root), and falls back to another scan root if the current root itself is gone. Backend uses `fs::read_dir` directly (instead of `Path::exists()` pre-check) so transient FS races immediately after a sibling rmdir don't false-positive as "missing".
- **Modal-aware key handling**: while any pane modal is open (Move / Delete confirm, Mkdir / Rename prompt, Blink), pane shortcuts are suppressed. Only Esc cancels and Y/Enter confirms the simple click-confirm dialogs.
- **IMAGETYP dropdown is the source of truth** for `is_master`: picking `MasterDark` writes `imagetyp='MasterDark'` AND `is_master=1` so the Equipment page's master detection (which filters by the imagetyp string) sees it.
- **Calibration-set pruning** runs on every metadata edit, not only IMAGETYP changes, so any cascade that empties a set cleans it up. Multi-member sets only prune when truly empty.

## Tests

Backend (`crates/athenaeum-core`): 229 tests, including the new ones added for this feature:

- `file_op::planner` — scan-root rejection, atomic-rename strategy detection, recursive directory expansion, destination-collision detection (single-file + directory-with-descendant collision).
- `file_op::executor` — atomic-rename hot-sync (with and without catalog `file_id`), full hierarchy delete, empty-dir cleanup with catalog sync.
- `services::operation_queue` — serial order, panic survival.
- `db::operations::bulk_metadata_tests` — cascade unlink (any-edit and imagetyp-only), empty-edit no-op, IS-MASTER from IMAGETYP, base/master variant flag flipping, rename prefix-swap correctness, frame memberships + used-in-frame-set rollup, override recompute (clears + persists), calibration-set pruning (master + multi-member).
- `fits_parser::stored_header::tests` — FITS card parsing, XISF FITSKeyword parsing, IMAGETYP / binning normalisation.

Frontend: TypeScript clean (`tsc --noEmit`), Vite production build green.

## What's intentionally NOT in scope

- Drag-and-drop between panes (removed — the user preferred shortcuts).
- Per-step undo/redo (originals-vs-current revert covers the actual debugging case).
- Frame-level revert of `bulk_update_calibration_metadata` propagation — only the calibration set itself reverts via the existing `calibration_set_originals` flow.
- Multi-step audit trail of metadata edits.
- Metadata-pane UI for `bulk_update_calibration_metadata` (Equipment page already has this; the dual-pane only edits frame-level metadata).
