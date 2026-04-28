# Archive Feature — Design Spec

**Date:** 2026-04-29
**Status:** Approved for implementation planning
**Scope:** v1 — single-frame-set archiving with move/copy/skip per calibration type, soft-archive catalog model, dedicated Archive page, and restore.

## 1. Problem & Goals

Astrophotographers accumulate large amounts of raw FITS data (lights + calibration frames) that they want to retire from active disk usage without losing the catalog metadata Athenaeum has built up. Today there is no way to archive a frame set: users must manually move files outside the app, which leaves the catalog with broken paths and no way to find the data later.

**Goals**

- Move (or move + copy) the files of a single frame set into one or more `.zip` archives stored in a user-chosen "archive" folder.
- Preserve the frame set's metadata in the catalog so users can browse archived imaging history offline.
- Make the operation **safe** — verify every byte before deleting any source, support cancel-with-rollback, and survive app crashes via DB-backed resume logs.
- Make the operation **restorable** — re-extract a zip back to disk and rewire `files.path` so the catalog matches reality.

**Non-goals (v1)**

- Cloud or remote-storage backends.
- Bulk archiving of multiple frame sets in one operation.
- A configurable zip-filename template.
- Searching the contents of existing zips for orphaned data.
- A corruption-check tool for archived zips.

## 2. User Flow

1. User opens a Frame Set's detail page (`FrameSetDetail.tsx`).
2. User clicks **Move and ZIP** — a new button placed next to the existing "Find new images" button on the FrameSetDetail toolbar. Both buttons are visible when the frame set is active. When the frame set is archived, both are replaced by a `[Restore]` button.
3. If `archive.root_path` is unset (or no longer exists), a modal asks the user to choose an archive folder. Picker uses `pickDirectory()`. Selection is saved to settings.
4. **Disposition dialog** appears with:
    - One row per calibration type that exists in the frame set's calibration chain (Flat / Dark / Bias / DarkFlat). Types not in the chain are not shown.
    - Per-type radio: **Move / Copy / Skip**. Default is **Skip** for every type.
    - For any calibration set linked to other (non-archived) frame sets, **Move is disabled** with an inline tooltip: *"Used by N other frame sets — only Copy is allowed."* Copy and Skip remain selectable.
    - Compression dropdown: **Store (no compression)** or **Deflate (smaller, slower)**. Default reads from `archive.compression` setting.
    - Estimated total archive size and a preview list of the zip filenames that will be produced.
    - Buttons: `[Start Archiving] [Cancel]`.
5. **Conflict check** — before queueing, app checks whether any planned zip filename already exists in the archive root. If so, modal: *"Overwrite / Add suffix / Cancel."*
6. **Execution** — operation is added to the active-archives list. Progress is reported in the same Tasks panel used by scan/analysis with stage labels:
    - `Copying 4/120` → `Verifying hashes` → `Building zip` → `Verifying zip` → `Deleting sources` → `Done`
7. User can **Cancel** at any point. Cancel triggers rollback (Section 5).
8. **On completion**: the frame set
    - Disappears from the Objects page.
    - Appears on the new **Archive** page.
    - Its FrameSetDetail page still loads but shows an "Archived" badge and disables operations needing raw files (re-analyze, plate-solve, export).

**One-at-a-time rule**: only one archive operation may be active. A second `Move and ZIP` click is blocked with a tooltip pointing to the active operation.

## 3. Data Model

### 3.1 Settings (key/value rows in existing `settings` table)

| Key                    | Type   | Default | Notes |
| ---------------------- | ------ | ------- | ----- |
| `archive.root_path`    | string | unset   | Absolute path to the archive folder. Single root in v1. |
| `archive.compression`  | string | `store` | `store` or `deflate`. |

### 3.2 Schema additions

```sql
-- Frame set markers
ALTER TABLE frames_set ADD COLUMN archived_at TIMESTAMP NULL;
ALTER TABLE frames_set ADD COLUMN archive_operation_id INTEGER NULL
    REFERENCES archive_operations(id);

-- File markers (set on every files row that ended up in a zip:
-- moved lights + moved/copied calibrations)
ALTER TABLE files ADD COLUMN archived_in_operation INTEGER NULL
    REFERENCES archive_operations(id);
ALTER TABLE files ADD COLUMN archive_zip_path TEXT NULL;
ALTER TABLE files ADD COLUMN archive_path_in_zip TEXT NULL;

-- One row per archive operation
CREATE TABLE archive_operations (
    id INTEGER PRIMARY KEY,
    frames_set_id INTEGER NOT NULL REFERENCES frames_set(id),
    archive_root_path TEXT NOT NULL,
    flats_disposition TEXT NULL,        -- "move" | "copy" | "skip" | NULL (not in chain)
    darks_disposition TEXT NULL,
    bias_disposition TEXT NULL,
    darkflats_disposition TEXT NULL,
    compression TEXT NOT NULL,           -- "store" | "deflate"
    status TEXT NOT NULL,                -- see state machine below
    started_at TIMESTAMP NOT NULL,
    finished_at TIMESTAMP NULL,
    error_message TEXT NULL
);

-- Frozen plan: one row per file the operation will touch.
-- Populated during the Plan stage, immutable afterward.
CREATE TABLE archive_operation_files (
    id INTEGER PRIMARY KEY,
    operation_id INTEGER NOT NULL REFERENCES archive_operations(id),
    file_id INTEGER NULL REFERENCES files(id),  -- nullable: row may be deleted later
    source_path TEXT NOT NULL,
    target_zip_path TEXT NOT NULL,              -- absolute zip path inside archive root
    target_path_in_zip TEXT NOT NULL,           -- e.g., "Lights/M31/2025-10-12/L_001.fits"
    expected_hash TEXT NOT NULL,                -- XXH3_64 of source file at plan time
    disposition TEXT NOT NULL,                  -- "move" | "copy"
    frame_role TEXT NOT NULL,                   -- "light" | "flat" | "dark" | "bias" | "darkflat"
    file_size_bytes INTEGER NOT NULL
);

-- Audit log: one row per (file, stage) pair, plus stage-level rows
CREATE TABLE archive_operation_steps (
    id INTEGER PRIMARY KEY,
    operation_id INTEGER NOT NULL REFERENCES archive_operations(id),
    operation_file_id INTEGER NULL REFERENCES archive_operation_files(id),
    stage TEXT NOT NULL,         -- "copy" | "verify_copy" | "zip_add" | "verify_zip" | "delete_source" | "finalize"
    status TEXT NOT NULL,        -- "pending" | "in_progress" | "done" | "failed" | "rolled_back"
    actual_hash TEXT NULL,       -- written during verify_copy
    error_message TEXT NULL,
    started_at TIMESTAMP NULL,
    completed_at TIMESTAMP NULL
);

CREATE INDEX idx_archive_files_op ON archive_operation_files(operation_id);
CREATE INDEX idx_archive_steps_op ON archive_operation_steps(operation_id, status);
```

### 3.3 State machine — `archive_operations.status`

```
planning  →  copying  →  verifying  →  zipping  →  zip_verifying
       ↓                                                  ↓
       ↓                                          deleting_sources
       ↓                                                  ↓
       ↓                                             finalizing
       ↓                                                  ↓
   cancelled                                          completed

  (any forward state) → rolling_back → rolled_back
  (any state) → failed (with error_message; followed by rolling_back)
```

### 3.4 Models (Rust)

`crates/athenaeum-core/src/archive/models.rs`:

```rust
pub struct ArchiveOperation { /* matches archive_operations row */ }
pub struct ArchiveOperationFile { /* matches archive_operation_files row */ }
pub struct ArchiveOperationStep { /* matches archive_operation_steps row */ }

pub enum ArchiveDisposition { Move, Copy, Skip }
pub enum ArchiveCompression { Store, Deflate }
pub enum ArchiveStage { Copy, VerifyCopy, ZipAdd, VerifyZip, DeleteSource, Finalize }
pub enum ArchiveStatus { Planning, Copying, Verifying, Zipping, ZipVerifying,
                         DeletingSources, Finalizing, Completed,
                         Cancelled, RollingBack, RolledBack, Failed }

pub struct ArchivePlan {
    pub operation_id: i64,
    pub files: Vec<ArchiveOperationFile>,
    pub zips: Vec<PlannedZip>,           // one per frame type produced
    pub shared_calibrations: Vec<SharedCalibrationWarning>,
    pub conflicts: Vec<ZipFilenameConflict>,
    pub total_size_bytes: u64,
}
```

Existing `FramesSet` and `FileRecord` models gain the new columns.

## 4. Stages & File Layout

### 4.1 Layout inside each zip (per Section 2 Q4)

```
<ObjectName>_<StartDate>_<EndDate>_<Telescope>_<Camera>_<FrameType>.zip
└── <ScanRootName>/<path/relative/to/scan_root>/<original_filename>
```

- `<ScanRootName>` is the basename of the scan root path (`/Photos/Lights` → `Lights`).
- If two scan roots have the same basename, append `_2`, `_3` etc. The mapping is recorded per-file in `archive_operation_files.target_path_in_zip`, so restore is unambiguous.
- Token fallbacks for the zip filename: missing telescope/camera/dates → `Unknown`.
- `<FrameType>` ∈ `{Lights, Flats, Darks, Bias, DarkFlats}`. One zip per frame type that has files.

### 4.2 Staging area

`<archive_root_path>/.athenaeum_staging/op_<operation_id>/`

- Hidden working directory.
- Holds copies of every file before zipping.
- Preserved through stages 2–6. Cleaned up at the end of stage 7 (Finalize) or by rollback.

### 4.3 Stages

| # | Stage             | Effect on disk                                                | Status during    |
|---|-------------------|---------------------------------------------------------------|------------------|
| 1 | Plan              | None. Writes `archive_operations` + `archive_operation_files` rows. | `planning`       |
| 2 | Copy              | Writes copies into staging.                                   | `copying`        |
| 3 | Verify copy       | Recomputes XXH3_64 of staging copy, compares to plan hash.    | `verifying`      |
| 4 | Build zip         | Builds final zip(s) in archive root from staging files.       | `zipping`        |
| 5 | Verify zip        | Opens each zip, asserts every expected entry is present.      | `zip_verifying`  |
| 6 | Delete sources    | Deletes original light files + (for "move" calibrations) original calibration files. **Point of no return for cheap rollback.** | `deleting_sources` |
| 7 | Finalize          | Sets `frames_set.archived_at`, `files.archive_*` columns, deletes staging. | `finalizing`     |

Cooperative cancellation: the worker checks the cancel flag between every per-file step. When set, it finishes the current step, then enters `rolling_back`.

### 4.4 Catalog row treatment

- **Moved files** (lights + move-mode calibrations): `files.archive_zip_path` and `files.archive_path_in_zip` are set; the row is otherwise preserved. `files.path` remains the original path (used by restore to remember "this is where it came from").
- **Copy-mode calibrations**: original `files`/`frames` rows are untouched. We do **not** create new catalog rows for the in-zip copies — they're just contents of the zip recorded in `archive_operation_files`.

## 5. Cancel & Rollback

### 5.1 Cancellation handle

`AppState.active_archives: HashMap<i64, ArchiveHandle>` where `ArchiveHandle { cancel_flag: Arc<AtomicBool> }`. Same pattern as `active_scans` / `active_analyses`.

### 5.2 Rollback by stage reached

| Stage at cancel/crash | Source files state             | Rollback action |
|-----------------------|---------------------------------|------------------|
| `planning`            | untouched                       | Set `status=cancelled`, delete `archive_operation_files` rows. |
| `copying`             | untouched                       | Delete every file already written under staging. Delete staging dir. |
| `verifying`           | untouched                       | Same as `copying`. |
| `zipping`             | untouched (zip not yet finalized) | Delete partial zips in archive root + staging dir. |
| `zip_verifying`       | untouched                       | Same as `zipping`. |
| `deleting_sources`    | some moved sources already gone | For each `delete_source` step with `status=done`: re-extract that file from staging back to its original path, hash-verify, mark `rolled_back`. After all restored, delete zips + staging dir. |
| `finalizing`          | sources gone, catalog being updated | Same as `deleting_sources` rollback, plus unset any `frames_set.archived_at` or `files.archived_in_operation` already written. |
| `completed`           | sources gone, staging gone      | **Cancel-time rollback NOT possible.** Use Restore (Section 7) to extract from zip. |

### 5.3 Rollback as its own state

Rollback enters `status=rolling_back` and records its own steps (`stage=delete_staging` or `stage=restore_source`). If the rollback itself crashes, the resume logic re-enters rollback by the same mechanism.

### 5.4 Resume on app startup

On launch, query `SELECT * FROM archive_operations WHERE status NOT IN ('completed', 'cancelled', 'rolled_back', 'failed')`. If any rows exist, show the **resume banner**:

> *"An archive operation was interrupted: M31 (Frame Set #42)."*
> `[Resume]` `[Roll back]` `[Decide later]`

- **Resume** — re-enter the worker. It reads `archive_operation_steps`, finds the first non-`done` step, and continues from there. Already-copied files with `verify_copy=done` are not re-copied.
- **Roll back** — enter the rollback path appropriate to the highest stage reached.
- **Decide later** — banner stays. The blocked frame set's "Move and ZIP" button is disabled with a tooltip pointing to the banner. New archive operations are blocked until this one resolves.

## 6. Backend Module Structure

### 6.1 New module: `crates/athenaeum-core/src/archive/`

```
archive/
├── mod.rs                  — public surface; re-exports
├── models.rs               — types from §3.4
├── planner.rs              — build_plan(conn, frames_set_id, dispositions, compression)
│                              -> ArchivePlan (without committing rows)
│                            commit_plan(conn, plan) -> i64 (operation_id)
├── executor.rs             — run_operation(conn, op_id, cancel_flag, progress)
│                            drives stages 2–7; cooperative cancel; idempotent per row
├── rollback.rs             — rollback_operation(conn, op_id, progress)
├── resume.rs               — find_unfinished_operations(conn),
│                            resume_operation(conn, op_id, ...)
├── staging.rs              — staging dir helpers
├── zip_writer.rs           — wrapper over `zip` crate (store + deflate)
├── zip_reader.rs           — verify_zip_contents(path, expected_entries)
└── shared_calibration.rs   — find_shared_calibration_sets(conn, frames_set_id, type)
```

### 6.2 Existing modules touched

- `crates/athenaeum-core/src/db/schema.rs` — add tables/columns from §3.2.
- `crates/athenaeum-core/src/db/operations.rs` — query helpers for archive tables.
- `crates/athenaeum-core/src/models.rs` — add new columns to `FramesSet` / `FileRecord`.
- `crates/athenaeum-core/src/duplicates/mod.rs` — reuse `compute_xxhash` (no changes).
- `crates/athenaeum-core/src/settings/mod.rs` — add `archive.root_path`, `archive.compression` keys.
- `crates/athenaeum-core/src/services/mod.rs` — add `active_archives: Arc<Mutex<HashMap<i64, ArchiveHandle>>>`.

### 6.3 New Tauri commands — `crates/athenaeum-tauri/src/commands/archive.rs`

```rust
#[tauri::command] pub async fn get_archive_settings(...) -> ArchiveSettings;
#[tauri::command] pub async fn set_archive_settings(...) -> ();

#[tauri::command]
pub async fn plan_archive_operation(
    frames_set_id: i64,
    dispositions: Dispositions,
    compression: ArchiveCompression,
) -> Result<ArchivePlanPreview, String>;
// Runs the planner WITHOUT committing rows. Returns target zip names + sizes
// + shared-calibration warnings + filename conflicts. Used by the disposition dialog.

#[tauri::command]
pub async fn start_archive_operation(
    frames_set_id: i64,
    dispositions: Dispositions,
    compression: ArchiveCompression,
    conflict_resolution: ConflictResolution,  // Overwrite | AddSuffix
) -> Result<i64, String>;
// Commits the plan, kicks off executor, returns operation_id.
// Progress emitted via `archive-progress` event.

#[tauri::command] pub async fn cancel_archive_operation(operation_id: i64) -> ();
#[tauri::command] pub async fn list_unfinished_archive_operations() -> Vec<ArchiveOperationSummary>;
#[tauri::command] pub async fn resume_archive_operation(operation_id: i64) -> ();
#[tauri::command] pub async fn rollback_archive_operation(operation_id: i64) -> ();

#[tauri::command] pub async fn list_archived_frame_sets() -> Vec<ArchivedFrameSetSummary>;

#[tauri::command]
pub async fn start_restore_operation(
    operation_id: i64,
    target_root_path: String,         // single-target restore (Section 7 Q8)
    overwrite_existing: bool,
    keep_zip_after_restore: bool,
) -> Result<i64, String>;

#[tauri::command]
pub async fn delete_archive(operation_id: i64) -> Result<(), String>;
```

Registered in `crates/athenaeum-tauri/src/lib.rs` invoke handler.

### 6.4 Web mirror — `crates/athenaeum-web/src/routes/archive.rs`

Same routes, using `SseProgressEmitter` (per workspace-architecture rule). Both backends must stay in sync.

### 6.5 Frontend

```
src/types/archive.ts                                — TS interfaces matching Rust
src/api/archive.ts                                  — desktop/web split (per src/api convention)
src/components/archive/
    ArchiveDispositionDialog.tsx                    — modal from §2 step 4
    ArchiveConflictDialog.tsx                       — overwrite/suffix/cancel
    ArchiveResumeBanner.tsx                         — top-of-app banner
    ArchiveProgress.tsx                             — plugged into existing Tasks panel
    RestoreDialog.tsx                               — single-target restore picker
src/pages/Archive.tsx                               — new sidebar entry
src/pages/FrameSetDetail.tsx                        — add Move and ZIP button + archived branch
src/components/Layout.tsx                           — add Archive sidebar link
```

## 7. Archive Page & Restore

### 7.1 Archive page (`/archive`)

Table sourced from `list_archived_frame_sets()`:

| Column           | Notes |
|------------------|-------|
| Object name      | from `frames_set.object` |
| Date range       | start–end of frames in the set |
| Telescope/Camera | aggregated |
| Frame counts     | per type (lights, flats, darks, bias, darkflats), each labelled "archived" or "skipped" |
| Archive size     | sum of zip file sizes |
| Archived at      | timestamp |
| Actions          | `[Open Detail]` `[Restore...]` `[Delete Archive...]` |

`[Open Detail]` opens `FrameSetDetail.tsx` in archived mode (badge visible, raw-file actions disabled, calibration tree shows in-zip vs skipped).

### 7.2 Restore flow

1. **Target dialog** — single folder picker (per Q8 answer):
    - Defaults to: original location (if all original paths still exist & target dir is writable), else first writable scan root, else nothing selected (Restore button disabled).
    - User can pick: original location, an existing scan root, an arbitrary folder via `pickDirectory()`, or **Add as new scan root** (creates a `scan_roots` row).
2. **Conflict check** — per file, check if a file already exists at the target. If any conflict: modal with `Overwrite all / Skip existing / Cancel`.
3. **Keep zip toggle** — in the dialog, default **off** (delete zip after successful restore). User opts in to keep.
4. **Stages**:
    - `extract` — write each file from zip(s) to its target path.
    - `verify` — recompute XXH3_64 of each extracted file, compare to `archive_operation_files.expected_hash`.
    - `update_catalog` — clear `frames_set.archived_at`; for every `files` row with `archived_in_operation = op_id`, set `files.path` to the new target path, clear archive markers.
    - `cleanup` — delete the zip(s) if `keep_zip_after_restore` was off.
5. **Verify failure** — keep the zip, mark restore failed, show error. Don't auto-rollback partial extracts.
6. **Cancel** — sets cancel flag. Rollback for restore = delete files extracted so far. Zip and catalog state untouched.
7. **Path rewriting** — when restore target differs from original, `files.path` is rewritten in DB to point to the new location. Frame sets that referenced moved calibrations regain working chains because the `calibration_set_to_frames` links never changed; only the underlying `files.path` did.

Restore reuses the same operation infrastructure: a row in a `restore_operations` table (or an extension of `archive_operations` with a `kind` column — final shape decided during implementation). Either approach produces the same UX.

### 7.3 Delete Archive

1. Confirmation modal: *"This will permanently delete `<zip files>` and remove this frame set's catalog entries. This cannot be undone."*
2. On confirm: delete zips, then delete the frame set + its frames + calibration links + archive_operations rows. The frame set row gets fully removed.

Blocked while another archive or restore operation is running.

## 8. Edge Cases (handled)

1. **Frame set has zero calibrations of a type** — type doesn't show in the disposition dialog.
2. **Frame set has no calibrations at all** — disposition dialog shows only compression + size + zip preview.
3. **Mixed master + single-file calibration sets** — planner treats them uniformly (every `files` row linked through `calibration_set_to_frames` is included).
4. **Source file missing at copy time** — operation `failed`, rollback runs, error message lists missing files.
5. **Insufficient disk space at archive root** — pre-flight check in planner: estimated total size ≤ available space minus 5% safety margin. Errors out before writing any rows.
6. **Same source file referenced twice in the plan** — planner deduplicates by `files.id`. Frame role priority for placement: `light` > `flat` > `darkflat` > `dark` > `bias`.
7. **Two scan roots with identical basenames** — planner appends `_2`, `_3` to the prefix.
8. **App killed during finalize** — sources already deleted; resume continues finalize (idempotent per row).
9. **`Delete Archive` while another operation is running** — blocked.
10. **Calibration set referenced after restore** — restore clears `archive_zip_path` and updates `files.path`; frame sets that referenced these calibrations regain working chains automatically.

## 9. Testing Plan

### 9.1 Unit tests (`crates/athenaeum-core/src/archive/`)

- `planner` — produces correct file list, correct zip names, correct path-in-zip, detects shared calibrations, deduplicates, computes total size.
- `staging` — directory creation/cleanup, path collision handling.
- `zip_writer` / `zip_reader` — round-trip: write entries, read back, verify list matches.
- `shared_calibration` — given a calibration set linked to N frame sets, returns correct "other frame sets" list.
- `rollback` — for each stage, given a partially-executed operation in DB, rollback restores the right state. Use temp dirs + in-memory SQLite.
- `resume` — kill mid-stage, restart, verify continues correctly.

### 9.2 Integration tests

- Real FITS files: small (KB) sample dataset committed under `crates/athenaeum-core/test-data/archive/` with one frame set + one master flat + one master dark. Test full archive → restore → re-archive cycle.
- Cancel midway through copy / midway through delete-sources / midway through finalize.
- Corrupt a staging file before `verify_copy` → verify failure path triggers rollback.
- Crash simulation: terminate worker mid-stage, reopen, resume.

### 9.3 Frontend manual test checklist

- Archive root unset → folder picker flow.
- Disposition dialog: shared calibration disables Move; default Skip everywhere.
- Conflict dialog flows (Overwrite / Add suffix / Cancel).
- Resume banner appears after simulated crash.
- Archive page lists archived sets correctly.
- Restore to original path; restore to picked path; verify `files.path` updated in DB.
- Delete Archive removes zips + catalog rows.

## 10. Out of Scope (deferred)

- Cloud / remote storage backends.
- Verifying contents of *existing* zips on the Archive page (corruption check).
- Bulk-archive multiple frame sets in one operation.
- Configurable zip filename template (token system already exists in export — could be reused later).
- Searching across in-zip file contents from the global catalog search (the soft-archived rows already cover metadata searches).

## 11. Open Implementation Decisions

- Whether restore reuses the `archive_operations` tables with a `kind` column or has its own `restore_operations` / `restore_operation_steps` pair. Either works; the implementation plan will pick one based on which produces less code duplication.
- Conflict-resolution dialog: per-zip toggle vs. one global choice. Default to global (single decision applies to all conflicting zips); per-file override can be added if requested.
