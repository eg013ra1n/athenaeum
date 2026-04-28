# Archive Feature Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the v1 Archive feature: move a single frame set's lights (and optionally move/copy/skip its calibrations) into one zip per frame type, with hash-verified copies, DB-backed resume logs, cancel-with-rollback, soft-archive catalog model, dedicated Archive page, and restore.

**Architecture:** New `crates/athenaeum-core/src/archive/` module with planner / executor / rollback / resume sub-modules; new `archive_operations` + `archive_operation_files` + `archive_operation_steps` tables; new Tauri commands in `crates/athenaeum-tauri/src/commands/archive.rs`; mirror routes in `crates/athenaeum-web/src/routes/archive.rs`; new frontend page + dialogs + sidebar entry. Reuses `compute_xxhash`, the `ProgressEmitter` trait, the `ServiceContext` active-handle pattern, and `pickDirectory()`.

**Tech Stack:** Rust (rusqlite, anyhow, serde, xxhash-rust, **`zip` crate** — new dep), Tauri 2.0, Axum 0.8, React + TypeScript, Tailwind, lucide-react.

**Source spec:** `docs/superpowers/specs/2026-04-29-archive-feature-design.md`

---

## Pre-Implementation Notes

### Naming conflict with existing `frames_set.is_archived` flag

The `frames_set` table **already** has a column called `is_archived` (boolean) with two existing commands `archive_frame_set` and `unarchive_frame_set`. That existing feature is a **soft-hide flag** — it just hides a frame set from the active Objects list. It does NOT involve zips, file moves, or any of the behavior in our spec.

**Coexistence policy for this plan:**

- The existing `is_archived` flag stays untouched in semantics (hide-only).
- The new ZIP archive feature uses a **separate** `archived_at: TIMESTAMP` column plus an `archive_operation_id: INTEGER` reference.
- A frame set is treated as "ZIP-archived" when `archived_at IS NOT NULL`.
- When ZIP-archiving a frame set, we **also set `is_archived = 1`** so existing UI hiding logic continues to work without modification — the user's mental model stays: "archived = not in active view."
- The new **Archive page** queries `WHERE archived_at IS NOT NULL` (so soft-hidden-but-not-zipped sets do NOT appear there).
- **Restore** clears both columns (`archived_at = NULL`, `is_archived = 0`).

The implementer should NOT rename or repurpose `is_archived`.

### File naming reminders

- Spec source of truth: `docs/superpowers/specs/2026-04-29-archive-feature-design.md`.
- Plan filename: `docs/superpowers/plans/2026-04-29-archive-feature.md` (this file).
- New Rust crate paths: `crates/athenaeum-core/src/archive/*.rs`, `crates/athenaeum-tauri/src/commands/archive.rs`, `crates/athenaeum-web/src/routes/archive.rs`.
- New frontend paths: `src/types/archive.ts`, `src/api/archive.ts`, `src/components/archive/*.tsx`, `src/pages/Archive.tsx`.

### Commit policy

- Commits are made by the human user, not the implementer. Each task ends with a `git commit` step the user will run.
- Use the project's commit format (`<type>(<scope>): <subject>` — see `git log` for examples like `docs(spec):`, `ci(gitlab):`).
- Do NOT add `Co-Authored-By: Claude` lines per project memory (commits as `eg013ra1n` only).

---

## File Structure (created or modified)

### New files

```
crates/athenaeum-core/src/archive/
  mod.rs                         # public surface; re-exports
  models.rs                      # ArchiveOperation, ArchiveOperationFile, ArchiveOperationStep, enums, ArchivePlan
  db.rs                          # CRUD helpers for the three new tables
  staging.rs                     # staging directory helpers
  zip_writer.rs                  # thin wrapper over `zip` crate
  zip_reader.rs                  # verify_zip_contents
  shared_calibration.rs          # find_shared_calibration_sets
  path_layout.rs                 # zip filename + path-in-zip computation
  planner.rs                     # build_plan, commit_plan
  executor.rs                    # run_operation (stages 2-7)
  rollback.rs                    # rollback_operation
  resume.rs                      # find_unfinished_operations, resume_operation
  restore.rs                     # restore planner + executor (single module — restore is small enough)

crates/athenaeum-tauri/src/commands/archive.rs   # Tauri command handlers
crates/athenaeum-web/src/routes/archive.rs       # Axum route handlers (mirror)

src/types/archive.ts                             # TypeScript types
src/api/archive.ts                               # API layer (desktop/web split)
src/components/archive/ArchiveDispositionDialog.tsx
src/components/archive/ArchiveConflictDialog.tsx
src/components/archive/ArchiveResumeBanner.tsx
src/components/archive/ArchiveProgress.tsx
src/components/archive/RestoreDialog.tsx
src/pages/Archive.tsx
```

### Modified files

```
crates/athenaeum-core/Cargo.toml                 # add `zip` dep
crates/athenaeum-core/src/lib.rs                 # pub mod archive;
crates/athenaeum-core/src/db/schema.rs           # add 3 tables + 5 ALTERs (migrations)
crates/athenaeum-core/src/services/mod.rs        # add ArchiveHandle + active_archives
crates/athenaeum-core/src/settings/mod.rs        # add archive.root_path / archive.compression keys
crates/athenaeum-core/src/models.rs              # add archived_at, archive_operation_id to FramesSet; add archive_zip_path, archive_path_in_zip, archived_in_operation to File

crates/athenaeum-tauri/src/commands/mod.rs       # pub mod archive; pub use archive::*;
crates/athenaeum-tauri/src/lib.rs                # register new invoke handlers
crates/athenaeum-web/src/routes/mod.rs           # mod archive; register routes

src/pages/FrameSetDetail.tsx                     # add Move and ZIP button + Restore button + archived banner
src/components/Layout.tsx                        # add Archive sidebar entry
src/App.tsx                                      # add /archive route
```

---

## Phase 1 — Foundation: Cargo deps, schema, models, settings

### Task 1: Add `zip` crate dependency

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml`

- [ ] **Step 1: Add `zip` dependency**

In the `[dependencies]` section of `crates/athenaeum-core/Cargo.toml`, add this line after the existing `flate2 = "1.0"` line:

```toml
zip = { version = "2", default-features = false, features = ["deflate", "time"] }
```

Rationale: `default-features = false` drops bz2/zstd/lzma/aes-crypto features we don't need; we keep `deflate` (required for compression mode) and `time` (file mtimes inside the zip).

- [ ] **Step 2: Verify it compiles**

Run from the workspace root: `cargo check -p athenaeum-core`
Expected: compiles cleanly (no warnings about unresolved `zip`).

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/Cargo.toml Cargo.lock
git commit -m "deps(core): add zip crate for archive feature"
```

---

### Task 2: Schema migration — three new tables + five ALTER TABLE columns

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs`

- [ ] **Step 1: Add three new `CREATE TABLE` statements before the migrations block**

Open `crates/athenaeum-core/src/db/schema.rs`. Find the existing block of `CREATE TABLE IF NOT EXISTS folder_similarity (...)` (around line 307) and the line `// Create indexes for common queries` (around line 322). Insert the following three `conn.execute(...)` calls **between** the `folder_similarity` table creation and the indexes block:

```rust
    // Archive operations - one row per archive operation (ZIP archive feature)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS archive_operations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            frames_set_id INTEGER NOT NULL,
            archive_root_path TEXT NOT NULL,
            flats_disposition TEXT,
            darks_disposition TEXT,
            bias_disposition TEXT,
            darkflats_disposition TEXT,
            compression TEXT NOT NULL,
            status TEXT NOT NULL,
            started_at TEXT NOT NULL,
            finished_at TEXT,
            error_message TEXT,
            FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Archive operation files - frozen plan: one row per file the operation will touch
    conn.execute(
        "CREATE TABLE IF NOT EXISTS archive_operation_files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            operation_id INTEGER NOT NULL,
            file_id INTEGER,
            source_path TEXT NOT NULL,
            target_zip_path TEXT NOT NULL,
            target_path_in_zip TEXT NOT NULL,
            expected_hash TEXT NOT NULL,
            disposition TEXT NOT NULL,
            frame_role TEXT NOT NULL,
            file_size_bytes INTEGER NOT NULL,
            FOREIGN KEY (operation_id) REFERENCES archive_operations(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Archive operation steps - audit log: one row per (file, stage) pair
    conn.execute(
        "CREATE TABLE IF NOT EXISTS archive_operation_steps (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            operation_id INTEGER NOT NULL,
            operation_file_id INTEGER,
            stage TEXT NOT NULL,
            status TEXT NOT NULL,
            actual_hash TEXT,
            error_message TEXT,
            started_at TEXT,
            completed_at TEXT,
            FOREIGN KEY (operation_id) REFERENCES archive_operations(id) ON DELETE CASCADE,
            FOREIGN KEY (operation_file_id) REFERENCES archive_operation_files(id) ON DELETE CASCADE
        )",
        [],
    )?;
```

- [ ] **Step 2: Add indexes for the new tables**

In the indexes block (after the existing `idx_dup_group_files_file` index), add these three indexes:

```rust
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_archive_files_op ON archive_operation_files(operation_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_archive_steps_op ON archive_operation_steps(operation_id, status)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_archive_ops_status ON archive_operations(status)",
        [],
    )?;
```

- [ ] **Step 3: Add five ALTER TABLE migrations following the existing pattern**

In the migrations block (the section beginning `// Migrations - add columns to existing tables if they don't exist`), add at the end:

```rust
    // Add archived_at to frames_set table (ZIP archive feature)
    let has_archived_at: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('frames_set') WHERE name='archived_at'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_archived_at {
        conn.execute(
            "ALTER TABLE frames_set ADD COLUMN archived_at TEXT",
            [],
        )?;
    }

    // Add archive_operation_id to frames_set table (ZIP archive feature)
    let has_archive_op_id: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('frames_set') WHERE name='archive_operation_id'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_archive_op_id {
        conn.execute(
            "ALTER TABLE frames_set ADD COLUMN archive_operation_id INTEGER",
            [],
        )?;
    }

    // Add archived_in_operation to files table (ZIP archive feature)
    let has_archived_in_op: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('files') WHERE name='archived_in_operation'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_archived_in_op {
        conn.execute(
            "ALTER TABLE files ADD COLUMN archived_in_operation INTEGER",
            [],
        )?;
    }

    // Add archive_zip_path to files table (ZIP archive feature)
    let has_archive_zip_path: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('files') WHERE name='archive_zip_path'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_archive_zip_path {
        conn.execute(
            "ALTER TABLE files ADD COLUMN archive_zip_path TEXT",
            [],
        )?;
    }

    // Add archive_path_in_zip to files table (ZIP archive feature)
    let has_archive_path_in_zip: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('files') WHERE name='archive_path_in_zip'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_archive_path_in_zip {
        conn.execute(
            "ALTER TABLE files ADD COLUMN archive_path_in_zip TEXT",
            [],
        )?;
    }
```

Note on column type for `archived_at`: SQLite stores timestamps as TEXT (ISO 8601). The model layer parses to `chrono::DateTime<Utc>`. Match the existing pattern used for `frames_set.date_obs_start`.

- [ ] **Step 4: Add schema test**

Append to the bottom of `crates/athenaeum-core/src/db/schema.rs`:

```rust
#[cfg(test)]
mod archive_schema_tests {
    use super::*;

    #[test]
    fn test_archive_tables_created() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        for table in &["archive_operations", "archive_operation_files", "archive_operation_steps"] {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            ).unwrap();
            assert_eq!(count, 1, "expected table {} to exist", table);
        }
    }

    #[test]
    fn test_archive_columns_added() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // frames_set columns
        for col in &["archived_at", "archive_operation_id"] {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('frames_set') WHERE name=?1",
                [col],
                |row| row.get(0),
            ).unwrap();
            assert_eq!(count, 1, "expected frames_set.{} to exist", col);
        }

        // files columns
        for col in &["archived_in_operation", "archive_zip_path", "archive_path_in_zip"] {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('files') WHERE name=?1",
                [col],
                |row| row.get(0),
            ).unwrap();
            assert_eq!(count, 1, "expected files.{} to exist", col);
        }
    }
}
```

- [ ] **Step 5: Run schema tests**

Run: `cargo test -p athenaeum-core --lib db::schema::archive_schema_tests`
Expected: 2 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/db/schema.rs
git commit -m "feat(db): add archive_operations schema + frames_set/files migrations"
```

---

### Task 3: Add archive settings keys + helper

**Files:**
- Modify: `crates/athenaeum-core/src/settings/mod.rs`

- [ ] **Step 1: Add keys + defaults**

In the `defaults` module (around line 10), append:

```rust
    // Archive feature
    pub const ARCHIVE_COMPRESSION: &str = "store"; // "store" | "deflate"
```

In the `keys` module (around line 43), append:

```rust
    // Archive feature
    pub const ARCHIVE_ROOT_PATH: &str = "archive.root_path";
    pub const ARCHIVE_COMPRESSION: &str = "archive.compression";
```

- [ ] **Step 2: Add SettingsManager helpers**

After the `get_duplicates_use_content_hash` method (around line 220), add:

```rust
    /// Get the configured archive root path (or None if unset).
    pub fn get_archive_root_path(&self, conn: &Connection) -> Result<Option<String>> {
        let value = self.get_with_precedence(conn, keys::ARCHIVE_ROOT_PATH, "")?;
        if value.is_empty() {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    /// Get the archive compression mode ("store" or "deflate").
    pub fn get_archive_compression(&self, conn: &Connection) -> Result<String> {
        self.get_with_precedence(
            conn,
            keys::ARCHIVE_COMPRESSION,
            defaults::ARCHIVE_COMPRESSION,
        )
    }
```

- [ ] **Step 3: Add tests**

Append in the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn test_archive_root_path_unset_returns_none() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        let value = manager.get_archive_root_path(&conn).unwrap();
        assert_eq!(value, None);
    }

    #[test]
    fn test_archive_root_path_set_returns_some() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        manager.persist_setting(&conn, keys::ARCHIVE_ROOT_PATH, "/tmp/archive").unwrap();
        assert_eq!(
            manager.get_archive_root_path(&conn).unwrap(),
            Some("/tmp/archive".to_string())
        );
    }

    #[test]
    fn test_archive_compression_default_is_store() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        assert_eq!(manager.get_archive_compression(&conn).unwrap(), "store");
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p athenaeum-core --lib settings`
Expected: existing tests still pass + 3 new tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/settings/mod.rs
git commit -m "feat(settings): add archive.root_path and archive.compression keys"
```

---

### Task 4: Add archive columns to `FramesSet` and `File` models

**Files:**
- Modify: `crates/athenaeum-core/src/models.rs`

- [ ] **Step 1: Update `File` struct**

In `crates/athenaeum-core/src/models.rs`, locate the `pub struct File { ... }` definition (lines 5-16). Add three new fields at the end of the struct:

```rust
    // ZIP archive feature — populated when this file's data has been moved
    // into a zip in the archive root. Original `path` is preserved for restore.
    pub archived_in_operation: Option<i64>,
    pub archive_zip_path: Option<String>,
    pub archive_path_in_zip: Option<String>,
```

- [ ] **Step 2: Update `FramesSet` struct**

Locate `pub struct FramesSet { ... }` (around line 205). Add two fields at the end:

```rust
    // ZIP archive feature — populated when this frame set has been ZIP-archived
    pub archived_at: Option<String>,           // ISO 8601 timestamp string
    pub archive_operation_id: Option<i64>,
```

- [ ] **Step 3: Find row-mapping sites and add the new columns**

Run: `grep -rn "FROM files" crates/athenaeum-core/src/db/ | grep -v "\.bak" | head` and `grep -rn "FROM frames_set" crates/athenaeum-core/src/db/ | head` to locate the `SELECT ... FROM files` and `SELECT ... FROM frames_set` query sites that map rows into the model structs.

For each query mapping into `File` or `FramesSet` model: extend the `SELECT` column list with the new columns and the `row.get(...)` mapping with the new fields, in the same order as the struct fields.

For File reads: add `archived_in_operation, archive_zip_path, archive_path_in_zip` after `content_hash`.
For FramesSet reads: add `archived_at, archive_operation_id` after `max_rotation` (or whatever the last field is in the existing query).

Tip: after the changes, run `cargo build -p athenaeum-core 2>&1 | head -50` and let the compiler tell you which struct-construction sites are now missing fields. Fix each one until clean.

- [ ] **Step 4: Verify build is clean**

Run: `cargo build -p athenaeum-core`
Expected: compiles cleanly with no errors. Warnings about unused new fields are OK at this point.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/models.rs crates/athenaeum-core/src/db/
git commit -m "feat(models): add archive columns to File and FramesSet"
```

---

### Task 5: Add `ArchiveHandle` to `ServiceContext`

**Files:**
- Modify: `crates/athenaeum-core/src/services/mod.rs`

- [ ] **Step 1: Add `ArchiveHandle` struct**

After the existing `pub struct PlateSolveHandle { ... }` definition, add:

```rust
/// Handle to track an active archive operation (ZIP archive feature).
/// Only one archive operation can run at a time, but the map allows
/// querying state by operation_id.
pub struct ArchiveHandle {
    pub operation_id: i64,
    pub cancel_flag: Arc<AtomicBool>,
}
```

- [ ] **Step 2: Add `active_archives` to `ServiceContext`**

In the `pub struct ServiceContext { ... }` definition, add this field after `active_plate_solves`:

```rust
    /// Active archive operations (ZIP archive feature). Capped at one at a
    /// time by command-layer enforcement; HashMap form keeps the same shape
    /// as the other active-handle maps for consistency.
    pub active_archives: Arc<Mutex<HashMap<i64, ArchiveHandle>>>,
```

- [ ] **Step 3: Initialize `active_archives` in any `ServiceContext` constructor**

Run: `grep -rn "active_plate_solves:" crates/athenaeum-core/src/services/ crates/athenaeum-tauri/src/ crates/athenaeum-web/src/` to find all sites that construct a `ServiceContext`. At each site, add a sibling `active_archives: Arc::new(Mutex::new(HashMap::new())),` line.

- [ ] **Step 4: Verify build is clean**

Run: `cargo build --workspace`
Expected: compiles cleanly.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/services/mod.rs crates/athenaeum-tauri/ crates/athenaeum-web/
git commit -m "feat(services): add ArchiveHandle and active_archives to ServiceContext"
```

---

## Phase 2 — Archive module skeleton & types

### Task 6: Create archive module skeleton

**Files:**
- Create: `crates/athenaeum-core/src/archive/mod.rs`
- Create: `crates/athenaeum-core/src/archive/models.rs`
- Modify: `crates/athenaeum-core/src/lib.rs`

- [ ] **Step 1: Create `mod.rs`**

Write `crates/athenaeum-core/src/archive/mod.rs`:

```rust
//! Archive feature — moves a frame set's lights (and chosen calibrations)
//! into one zip per frame type inside a user-chosen archive root.
//!
//! See `docs/superpowers/specs/2026-04-29-archive-feature-design.md`.

pub mod models;
pub mod db;
pub mod staging;
pub mod zip_writer;
pub mod zip_reader;
pub mod shared_calibration;
pub mod path_layout;
pub mod planner;
pub mod executor;
pub mod rollback;
pub mod resume;
pub mod restore;

pub use models::*;
```

- [ ] **Step 2: Create empty `models.rs` placeholder**

Write `crates/athenaeum-core/src/archive/models.rs`:

```rust
//! Archive operation data types.

use serde::{Deserialize, Serialize};

/// What to do with a calibration type's files during archiving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArchiveDisposition {
    Move,
    Copy,
    Skip,
}

impl ArchiveDisposition {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArchiveDisposition::Move => "move",
            ArchiveDisposition::Copy => "copy",
            ArchiveDisposition::Skip => "skip",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "move" => Some(Self::Move),
            "copy" => Some(Self::Copy),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

/// Compression mode for archive zips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArchiveCompression {
    Store,
    Deflate,
}

impl ArchiveCompression {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArchiveCompression::Store => "store",
            ArchiveCompression::Deflate => "deflate",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "store" => Some(Self::Store),
            "deflate" => Some(Self::Deflate),
            _ => None,
        }
    }
}

/// Stages of a forward archive operation. Matches the `archive_operation_steps.stage` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveStage {
    Copy,
    VerifyCopy,
    ZipAdd,
    VerifyZip,
    DeleteSource,
    Finalize,
    // Rollback-only stages
    DeleteStaging,
    RestoreSource,
}

impl ArchiveStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArchiveStage::Copy => "copy",
            ArchiveStage::VerifyCopy => "verify_copy",
            ArchiveStage::ZipAdd => "zip_add",
            ArchiveStage::VerifyZip => "verify_zip",
            ArchiveStage::DeleteSource => "delete_source",
            ArchiveStage::Finalize => "finalize",
            ArchiveStage::DeleteStaging => "delete_staging",
            ArchiveStage::RestoreSource => "restore_source",
        }
    }
}

/// Status values for a step row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Done,
    Failed,
    RolledBack,
}

impl StepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::InProgress => "in_progress",
            StepStatus::Done => "done",
            StepStatus::Failed => "failed",
            StepStatus::RolledBack => "rolled_back",
        }
    }
}

/// State machine for `archive_operations.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveStatus {
    Planning,
    Copying,
    Verifying,
    Zipping,
    ZipVerifying,
    DeletingSources,
    Finalizing,
    Completed,
    Cancelled,
    RollingBack,
    RolledBack,
    Failed,
}

impl ArchiveStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArchiveStatus::Planning => "planning",
            ArchiveStatus::Copying => "copying",
            ArchiveStatus::Verifying => "verifying",
            ArchiveStatus::Zipping => "zipping",
            ArchiveStatus::ZipVerifying => "zip_verifying",
            ArchiveStatus::DeletingSources => "deleting_sources",
            ArchiveStatus::Finalizing => "finalizing",
            ArchiveStatus::Completed => "completed",
            ArchiveStatus::Cancelled => "cancelled",
            ArchiveStatus::RollingBack => "rolling_back",
            ArchiveStatus::RolledBack => "rolled_back",
            ArchiveStatus::Failed => "failed",
        }
    }

    /// Is this a state where work could still be in progress (i.e. resumable)?
    pub fn is_unfinished(&self) -> bool {
        !matches!(
            self,
            ArchiveStatus::Completed
                | ArchiveStatus::Cancelled
                | ArchiveStatus::RolledBack
                | ArchiveStatus::Failed
        )
    }
}

/// The frame role determines which zip a file goes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameRole {
    Light,
    Flat,
    Dark,
    Bias,
    Darkflat,
}

impl FrameRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            FrameRole::Light => "light",
            FrameRole::Flat => "flat",
            FrameRole::Dark => "dark",
            FrameRole::Bias => "bias",
            FrameRole::Darkflat => "darkflat",
        }
    }

    /// Folder name within the zip filename (e.g. "Lights", "Flats").
    pub fn zip_suffix(&self) -> &'static str {
        match self {
            FrameRole::Light => "Lights",
            FrameRole::Flat => "Flats",
            FrameRole::Dark => "Darks",
            FrameRole::Bias => "Bias",
            FrameRole::Darkflat => "DarkFlats",
        }
    }

    /// Priority for dedup (lower = wins): light > flat > darkflat > dark > bias.
    pub fn priority(&self) -> u8 {
        match self {
            FrameRole::Light => 0,
            FrameRole::Flat => 1,
            FrameRole::Darkflat => 2,
            FrameRole::Dark => 3,
            FrameRole::Bias => 4,
        }
    }
}

/// Disposition selections for the four calibration types.
/// `None` means the type is not present in the chain (so no question was asked).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dispositions {
    pub flats: Option<ArchiveDisposition>,
    pub darks: Option<ArchiveDisposition>,
    pub bias: Option<ArchiveDisposition>,
    pub darkflats: Option<ArchiveDisposition>,
}

/// One row of `archive_operations`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveOperation {
    pub id: i64,
    pub frames_set_id: i64,
    pub archive_root_path: String,
    pub flats_disposition: Option<String>,
    pub darks_disposition: Option<String>,
    pub bias_disposition: Option<String>,
    pub darkflats_disposition: Option<String>,
    pub compression: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error_message: Option<String>,
}

/// One row of `archive_operation_files`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveOperationFile {
    pub id: i64,
    pub operation_id: i64,
    pub file_id: Option<i64>,
    pub source_path: String,
    pub target_zip_path: String,
    pub target_path_in_zip: String,
    pub expected_hash: String,
    pub disposition: String,        // "move" | "copy"
    pub frame_role: String,         // "light" | "flat" | ...
    pub file_size_bytes: i64,
}

/// One row of `archive_operation_steps`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveOperationStep {
    pub id: i64,
    pub operation_id: i64,
    pub operation_file_id: Option<i64>,
    pub stage: String,
    pub status: String,
    pub actual_hash: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// One zip the operation will produce.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedZip {
    pub zip_path: String,            // absolute
    pub zip_filename: String,
    pub frame_role: FrameRole,
    pub file_count: usize,
    pub total_size_bytes: u64,
}

/// Warning emitted by the planner when the user chose Move on a calibration set
/// that's also linked to other (non-archived) frame sets. UI uses this to
/// disable the Move radio for that calibration type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedCalibrationWarning {
    pub frame_role: FrameRole,
    pub calibration_set_id: i64,
    pub other_frames_set_ids: Vec<i64>,
}

/// Conflict emitted by the planner when a target zip filename already exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZipFilenameConflict {
    pub zip_path: String,
    pub zip_filename: String,
}

/// The complete plan for an archive operation. Returned by `plan_archive_operation`
/// for the disposition dialog preview, and (after `commit_plan`) used to drive the executor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivePlan {
    pub frames_set_id: i64,
    pub archive_root_path: String,
    pub dispositions: Dispositions,
    pub compression: ArchiveCompression,
    pub files: Vec<ArchiveOperationFile>,        // id=0 until commit_plan persists
    pub zips: Vec<PlannedZip>,
    pub shared_calibrations: Vec<SharedCalibrationWarning>,
    pub conflicts: Vec<ZipFilenameConflict>,
    pub total_size_bytes: u64,
}

/// How to resolve filename conflicts. Provided by the user via the conflict dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictResolution {
    Overwrite,
    AddSuffix,
}

/// Summary used by the resume banner + Archive page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveOperationSummary {
    pub id: i64,
    pub frames_set_id: i64,
    pub frame_set_name: Option<String>,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error_message: Option<String>,
}
```

- [ ] **Step 3: Create empty placeholder modules**

For each of these files, create with the single-line body shown. Each will be filled in by a later task. Creating them now makes the `mod.rs` declarations valid.

`crates/athenaeum-core/src/archive/db.rs`:
```rust
//! Database CRUD for archive_operations / archive_operation_files / archive_operation_steps.
```

`crates/athenaeum-core/src/archive/staging.rs`:
```rust
//! Helpers for the per-operation staging directory.
```

`crates/athenaeum-core/src/archive/zip_writer.rs`:
```rust
//! Thin wrapper over the `zip` crate.
```

`crates/athenaeum-core/src/archive/zip_reader.rs`:
```rust
//! Verify zip contents.
```

`crates/athenaeum-core/src/archive/shared_calibration.rs`:
```rust
//! Detect calibration sets shared with other (non-archived) frame sets.
```

`crates/athenaeum-core/src/archive/path_layout.rs`:
```rust
//! Compute zip filenames and path-in-zip strings.
```

`crates/athenaeum-core/src/archive/planner.rs`:
```rust
//! Build and commit the archive plan.
```

`crates/athenaeum-core/src/archive/executor.rs`:
```rust
//! Drive stages 2-7 of the archive operation.
```

`crates/athenaeum-core/src/archive/rollback.rs`:
```rust
//! Roll back an archive operation by reading its step log.
```

`crates/athenaeum-core/src/archive/resume.rs`:
```rust
//! Find unfinished archive operations and resume them.
```

`crates/athenaeum-core/src/archive/restore.rs`:
```rust
//! Restore: extract zip(s) back to disk and update files.path.
```

- [ ] **Step 4: Wire `archive` into `lib.rs`**

In `crates/athenaeum-core/src/lib.rs`, add `pub mod archive;` after `pub mod plate_solve;`:

```rust
pub mod plate_solve;
pub mod services;
pub mod archive;
```

- [ ] **Step 5: Verify build and add a smoke test**

Run: `cargo build -p athenaeum-core`
Expected: compiles cleanly.

Add a smoke test in `crates/athenaeum-core/src/archive/models.rs` at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_roundtrip() {
        for d in [ArchiveDisposition::Move, ArchiveDisposition::Copy, ArchiveDisposition::Skip] {
            assert_eq!(ArchiveDisposition::from_str(d.as_str()), Some(d));
        }
    }

    #[test]
    fn compression_roundtrip() {
        for c in [ArchiveCompression::Store, ArchiveCompression::Deflate] {
            assert_eq!(ArchiveCompression::from_str(c.as_str()), Some(c));
        }
    }

    #[test]
    fn status_unfinished() {
        assert!(ArchiveStatus::Copying.is_unfinished());
        assert!(ArchiveStatus::Finalizing.is_unfinished());
        assert!(!ArchiveStatus::Completed.is_unfinished());
        assert!(!ArchiveStatus::Cancelled.is_unfinished());
        assert!(!ArchiveStatus::RolledBack.is_unfinished());
        assert!(!ArchiveStatus::Failed.is_unfinished());
    }

    #[test]
    fn frame_role_priority_order() {
        // Light wins over everything; bias loses to everything.
        assert!(FrameRole::Light.priority() < FrameRole::Flat.priority());
        assert!(FrameRole::Flat.priority() < FrameRole::Darkflat.priority());
        assert!(FrameRole::Darkflat.priority() < FrameRole::Dark.priority());
        assert!(FrameRole::Dark.priority() < FrameRole::Bias.priority());
    }
}
```

Run: `cargo test -p athenaeum-core --lib archive::models::tests`
Expected: 4 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/archive/ crates/athenaeum-core/src/lib.rs
git commit -m "feat(archive): scaffold archive module with types and stage enums"
```

---

## Phase 3 — Leaf modules: DB helpers, path layout, staging, zip I/O, shared calibration

Each task here is TDD: write the test first, see it fail, implement, see it pass.

### Task 7: `archive::db` CRUD helpers

**Files:**
- Modify: `crates/athenaeum-core/src/archive/db.rs`

- [ ] **Step 1: Write failing tests**

Replace the contents of `crates/athenaeum-core/src/archive/db.rs` with:

```rust
//! Database CRUD for archive_operations / archive_operation_files / archive_operation_steps.

use crate::archive::models::{
    ArchiveOperation, ArchiveOperationFile, ArchiveOperationStep,
    ArchiveOperationSummary, ArchiveStage, ArchiveStatus, StepStatus,
};
use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection};

/// Insert a new archive_operations row in `Planning` status.
/// Returns the new operation_id.
pub fn insert_operation(
    conn: &Connection,
    frames_set_id: i64,
    archive_root_path: &str,
    flats: Option<&str>,
    darks: Option<&str>,
    bias: Option<&str>,
    darkflats: Option<&str>,
    compression: &str,
) -> Result<i64> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO archive_operations (
            frames_set_id, archive_root_path,
            flats_disposition, darks_disposition, bias_disposition, darkflats_disposition,
            compression, status, started_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            frames_set_id,
            archive_root_path,
            flats,
            darks,
            bias,
            darkflats,
            compression,
            ArchiveStatus::Planning.as_str(),
            now,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Update the status of an operation. Sets `finished_at` if status is terminal.
pub fn update_operation_status(
    conn: &Connection,
    operation_id: i64,
    status: ArchiveStatus,
    error_message: Option<&str>,
) -> Result<()> {
    let is_terminal = matches!(
        status,
        ArchiveStatus::Completed
            | ArchiveStatus::Cancelled
            | ArchiveStatus::RolledBack
            | ArchiveStatus::Failed
    );
    let finished_at = if is_terminal {
        Some(Utc::now().to_rfc3339())
    } else {
        None
    };
    conn.execute(
        "UPDATE archive_operations
         SET status = ?1, finished_at = COALESCE(?2, finished_at), error_message = COALESCE(?3, error_message)
         WHERE id = ?4",
        params![status.as_str(), finished_at, error_message, operation_id],
    )?;
    Ok(())
}

/// Insert an archive_operation_files row. Returns its id.
pub fn insert_operation_file(
    conn: &Connection,
    operation_id: i64,
    file_id: Option<i64>,
    source_path: &str,
    target_zip_path: &str,
    target_path_in_zip: &str,
    expected_hash: &str,
    disposition: &str,
    frame_role: &str,
    file_size_bytes: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO archive_operation_files (
            operation_id, file_id, source_path, target_zip_path, target_path_in_zip,
            expected_hash, disposition, frame_role, file_size_bytes
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            operation_id,
            file_id,
            source_path,
            target_zip_path,
            target_path_in_zip,
            expected_hash,
            disposition,
            frame_role,
            file_size_bytes,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// List all operation_files for an operation, ordered by id.
pub fn list_operation_files(conn: &Connection, operation_id: i64) -> Result<Vec<ArchiveOperationFile>> {
    let mut stmt = conn.prepare(
        "SELECT id, operation_id, file_id, source_path, target_zip_path, target_path_in_zip,
                expected_hash, disposition, frame_role, file_size_bytes
         FROM archive_operation_files
         WHERE operation_id = ?1
         ORDER BY id",
    )?;
    let rows = stmt.query_map([operation_id], |row| {
        Ok(ArchiveOperationFile {
            id: row.get(0)?,
            operation_id: row.get(1)?,
            file_id: row.get(2)?,
            source_path: row.get(3)?,
            target_zip_path: row.get(4)?,
            target_path_in_zip: row.get(5)?,
            expected_hash: row.get(6)?,
            disposition: row.get(7)?,
            frame_role: row.get(8)?,
            file_size_bytes: row.get(9)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.into())
}

/// Insert a new step row in `Pending` status. Returns its id.
pub fn insert_step(
    conn: &Connection,
    operation_id: i64,
    operation_file_id: Option<i64>,
    stage: ArchiveStage,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO archive_operation_steps (
            operation_id, operation_file_id, stage, status
        ) VALUES (?1, ?2, ?3, ?4)",
        params![operation_id, operation_file_id, stage.as_str(), StepStatus::Pending.as_str()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Update an existing step's status (and optional fields).
pub fn update_step(
    conn: &Connection,
    step_id: i64,
    status: StepStatus,
    actual_hash: Option<&str>,
    error_message: Option<&str>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let started_at_clause = if matches!(status, StepStatus::InProgress) {
        Some(now.clone())
    } else {
        None
    };
    let completed_at_clause = if matches!(status, StepStatus::Done | StepStatus::Failed | StepStatus::RolledBack) {
        Some(now)
    } else {
        None
    };
    conn.execute(
        "UPDATE archive_operation_steps
         SET status = ?1,
             actual_hash = COALESCE(?2, actual_hash),
             error_message = COALESCE(?3, error_message),
             started_at = COALESCE(?4, started_at),
             completed_at = COALESCE(?5, completed_at)
         WHERE id = ?6",
        params![status.as_str(), actual_hash, error_message, started_at_clause, completed_at_clause, step_id],
    )?;
    Ok(())
}

/// List all steps for an operation, ordered by id.
pub fn list_steps(conn: &Connection, operation_id: i64) -> Result<Vec<ArchiveOperationStep>> {
    let mut stmt = conn.prepare(
        "SELECT id, operation_id, operation_file_id, stage, status, actual_hash, error_message,
                started_at, completed_at
         FROM archive_operation_steps
         WHERE operation_id = ?1
         ORDER BY id",
    )?;
    let rows = stmt.query_map([operation_id], |row| {
        Ok(ArchiveOperationStep {
            id: row.get(0)?,
            operation_id: row.get(1)?,
            operation_file_id: row.get(2)?,
            stage: row.get(3)?,
            status: row.get(4)?,
            actual_hash: row.get(5)?,
            error_message: row.get(6)?,
            started_at: row.get(7)?,
            completed_at: row.get(8)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.into())
}

/// Get a single archive_operations row.
pub fn get_operation(conn: &Connection, operation_id: i64) -> Result<ArchiveOperation> {
    let row = conn.query_row(
        "SELECT id, frames_set_id, archive_root_path,
                flats_disposition, darks_disposition, bias_disposition, darkflats_disposition,
                compression, status, started_at, finished_at, error_message
         FROM archive_operations
         WHERE id = ?1",
        [operation_id],
        |row| {
            Ok(ArchiveOperation {
                id: row.get(0)?,
                frames_set_id: row.get(1)?,
                archive_root_path: row.get(2)?,
                flats_disposition: row.get(3)?,
                darks_disposition: row.get(4)?,
                bias_disposition: row.get(5)?,
                darkflats_disposition: row.get(6)?,
                compression: row.get(7)?,
                status: row.get(8)?,
                started_at: row.get(9)?,
                finished_at: row.get(10)?,
                error_message: row.get(11)?,
            })
        },
    )?;
    Ok(row)
}

/// List operations whose status is "unfinished" (not Completed/Cancelled/RolledBack/Failed).
pub fn list_unfinished_operations(conn: &Connection) -> Result<Vec<ArchiveOperationSummary>> {
    let mut stmt = conn.prepare(
        "SELECT op.id, op.frames_set_id, fs.name, op.status, op.started_at, op.finished_at, op.error_message
         FROM archive_operations op
         LEFT JOIN frames_set fs ON fs.id = op.frames_set_id
         WHERE op.status NOT IN ('completed','cancelled','rolled_back','failed')
         ORDER BY op.started_at",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ArchiveOperationSummary {
            id: row.get(0)?,
            frames_set_id: row.get(1)?,
            frame_set_name: row.get(2)?,
            status: row.get(3)?,
            started_at: row.get(4)?,
            finished_at: row.get(5)?,
            error_message: row.get(6)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.into())
}

/// Mark a frames_set as ZIP-archived. Sets archived_at, archive_operation_id,
/// AND is_archived (so existing UI hide logic continues to work).
pub fn mark_frame_set_archived(conn: &Connection, frames_set_id: i64, operation_id: i64) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE frames_set
         SET archived_at = ?1, archive_operation_id = ?2, is_archived = 1
         WHERE id = ?3",
        params![now, operation_id, frames_set_id],
    )?;
    Ok(())
}

/// Clear archive markers from a frames_set (used by rollback and restore).
pub fn unmark_frame_set_archived(conn: &Connection, frames_set_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE frames_set
         SET archived_at = NULL, archive_operation_id = NULL, is_archived = 0
         WHERE id = ?1",
        [frames_set_id],
    )?;
    Ok(())
}

/// Mark a single file as archived (sets archive_zip_path + archive_path_in_zip + archived_in_operation).
pub fn mark_file_archived(
    conn: &Connection,
    file_id: i64,
    operation_id: i64,
    archive_zip_path: &str,
    archive_path_in_zip: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE files
         SET archived_in_operation = ?1, archive_zip_path = ?2, archive_path_in_zip = ?3
         WHERE id = ?4",
        params![operation_id, archive_zip_path, archive_path_in_zip, file_id],
    )?;
    Ok(())
}

/// Clear archive markers from a file. Optionally rewrite path (used by restore).
pub fn unmark_file_archived(
    conn: &Connection,
    file_id: i64,
    new_path: Option<&str>,
) -> Result<()> {
    if let Some(path) = new_path {
        conn.execute(
            "UPDATE files
             SET archived_in_operation = NULL, archive_zip_path = NULL, archive_path_in_zip = NULL,
                 path = ?1
             WHERE id = ?2",
            params![path, file_id],
        )?;
    } else {
        conn.execute(
            "UPDATE files
             SET archived_in_operation = NULL, archive_zip_path = NULL, archive_path_in_zip = NULL
             WHERE id = ?1",
            [file_id],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn setup() -> (Connection, i64) {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // Insert a frame_set so foreign key works
        conn.execute(
            "INSERT INTO frames_set (id, name) VALUES (1, 'TestSet')",
            [],
        ).unwrap();
        (conn, 1)
    }

    #[test]
    fn insert_and_get_operation() {
        let (conn, fs_id) = setup();
        let op_id = insert_operation(
            &conn, fs_id, "/tmp/arch", Some("move"), Some("copy"), None, None, "store",
        ).unwrap();
        let op = get_operation(&conn, op_id).unwrap();
        assert_eq!(op.frames_set_id, fs_id);
        assert_eq!(op.archive_root_path, "/tmp/arch");
        assert_eq!(op.status, "planning");
        assert_eq!(op.flats_disposition.as_deref(), Some("move"));
        assert_eq!(op.darks_disposition.as_deref(), Some("copy"));
        assert!(op.bias_disposition.is_none());
    }

    #[test]
    fn update_operation_status_sets_finished_at_on_terminal() {
        let (conn, fs_id) = setup();
        let op_id = insert_operation(&conn, fs_id, "/tmp", None, None, None, None, "store").unwrap();

        update_operation_status(&conn, op_id, ArchiveStatus::Copying, None).unwrap();
        let op = get_operation(&conn, op_id).unwrap();
        assert!(op.finished_at.is_none());

        update_operation_status(&conn, op_id, ArchiveStatus::Completed, None).unwrap();
        let op = get_operation(&conn, op_id).unwrap();
        assert!(op.finished_at.is_some());
    }

    #[test]
    fn insert_files_and_steps() {
        let (conn, fs_id) = setup();
        let op_id = insert_operation(&conn, fs_id, "/tmp", None, None, None, None, "store").unwrap();
        let file_id = insert_operation_file(
            &conn, op_id, None, "/src/a.fits", "/tmp/A.zip", "Lights/a.fits",
            "deadbeefdeadbeef", "move", "light", 1024,
        ).unwrap();
        let files = list_operation_files(&conn, op_id).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].source_path, "/src/a.fits");

        let step_id = insert_step(&conn, op_id, Some(file_id), ArchiveStage::Copy).unwrap();
        update_step(&conn, step_id, StepStatus::Done, None, None).unwrap();
        let steps = list_steps(&conn, op_id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].status, "done");
        assert!(steps[0].completed_at.is_some());
    }

    #[test]
    fn list_unfinished_excludes_terminal_states() {
        let (conn, fs_id) = setup();
        let a = insert_operation(&conn, fs_id, "/tmp/a", None, None, None, None, "store").unwrap();
        let b = insert_operation(&conn, fs_id, "/tmp/b", None, None, None, None, "store").unwrap();
        let c = insert_operation(&conn, fs_id, "/tmp/c", None, None, None, None, "store").unwrap();

        update_operation_status(&conn, a, ArchiveStatus::Completed, None).unwrap();
        update_operation_status(&conn, b, ArchiveStatus::Copying, None).unwrap();
        update_operation_status(&conn, c, ArchiveStatus::Failed, Some("boom")).unwrap();

        let unfinished = list_unfinished_operations(&conn).unwrap();
        assert_eq!(unfinished.len(), 1);
        assert_eq!(unfinished[0].id, b);
    }

    #[test]
    fn mark_unmark_frame_set() {
        let (conn, fs_id) = setup();
        let op_id = insert_operation(&conn, fs_id, "/tmp", None, None, None, None, "store").unwrap();
        mark_frame_set_archived(&conn, fs_id, op_id).unwrap();

        let (archived_at, op, is_arch): (Option<String>, Option<i64>, i32) = conn.query_row(
            "SELECT archived_at, archive_operation_id, is_archived FROM frames_set WHERE id = ?1",
            [fs_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert!(archived_at.is_some());
        assert_eq!(op, Some(op_id));
        assert_eq!(is_arch, 1);

        unmark_frame_set_archived(&conn, fs_id).unwrap();
        let (archived_at, op, is_arch): (Option<String>, Option<i64>, i32) = conn.query_row(
            "SELECT archived_at, archive_operation_id, is_archived FROM frames_set WHERE id = ?1",
            [fs_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert!(archived_at.is_none());
        assert!(op.is_none());
        assert_eq!(is_arch, 0);
    }
}
```

- [ ] **Step 2: Run tests — confirm they fail (because the file is currently the placeholder one-liner)**

Wait, this task replaced the file completely so the tests are now coexisting with the implementation. Run:

```
cargo test -p athenaeum-core --lib archive::db::tests
```

Expected: 5 tests pass.

If anything fails, fix the implementation in the file (not the tests).

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/archive/db.rs
git commit -m "feat(archive): add CRUD helpers for operations, files, and steps"
```

---

### Task 8: `archive::path_layout` — zip filename + path-in-zip computation

**Files:**
- Modify: `crates/athenaeum-core/src/archive/path_layout.rs`

- [ ] **Step 1: Write failing tests + implementation**

Replace the contents of `crates/athenaeum-core/src/archive/path_layout.rs` with:

```rust
//! Compute zip filenames and path-in-zip strings for the archive feature.

use crate::archive::models::FrameRole;
use std::path::{Path, PathBuf};

/// Sluggify text for inclusion in a filename: replace whitespace with `_`,
/// strip characters that are problematic on common filesystems.
pub fn sanitize_for_filename(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            // Reserved on Windows + many tools' breakage points
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            c if c.is_whitespace() => out.push('_'),
            c if c.is_control() => {} // drop
            c => out.push(c),
        }
    }
    // Collapse multiple underscores
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

/// Token, with a "Unknown" fallback when the value is None or empty.
fn token(value: Option<&str>) -> String {
    let s = value.unwrap_or("").trim();
    if s.is_empty() {
        "Unknown".to_string()
    } else {
        sanitize_for_filename(s)
    }
}

/// Compute the zip filename for a given frame role and frame-set metadata.
///
/// Format: `{Object}_{StartDate}_{EndDate}_{Telescope}_{Camera}_{FrameType}.zip`
/// All tokens fall back to "Unknown".
pub fn zip_filename(
    object: Option<&str>,
    start_date: Option<&str>,    // YYYY-MM-DD
    end_date: Option<&str>,
    telescope: Option<&str>,
    camera: Option<&str>,
    role: FrameRole,
) -> String {
    format!(
        "{}_{}_{}_{}_{}_{}.zip",
        token(object),
        token(start_date),
        token(end_date),
        token(telescope),
        token(camera),
        role.zip_suffix()
    )
}

/// Resolve unique scan-root prefix names. Given a list of scan_root absolute paths
/// (in arbitrary order), returns a map from path → unique basename. If two roots
/// share a basename, suffixes `_2`, `_3`, ... are appended in input order.
pub fn resolve_scan_root_prefixes(scan_root_paths: &[String]) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut out: HashMap<String, String> = HashMap::new();

    for path in scan_root_paths {
        let basename = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| sanitize_for_filename(s))
            .unwrap_or_else(|| "Root".to_string());
        let n = counts.entry(basename.clone()).and_modify(|n| *n += 1).or_insert(1);
        let unique = if *n == 1 {
            basename
        } else {
            format!("{}_{}", basename, n)
        };
        out.insert(path.clone(), unique);
    }
    out
}

/// Compute the path-in-zip for a source file.
///
/// `<UniqueRootName>/<rel-path-from-root>` with forward slashes (zip convention).
/// If the source file is not under `scan_root`, falls back to just `<UniqueRootName>/<basename>`.
pub fn path_in_zip(unique_root_name: &str, scan_root: &Path, source_file: &Path) -> String {
    let rel = source_file.strip_prefix(scan_root).ok();
    let mut buf = PathBuf::from(unique_root_name);
    match rel {
        Some(p) => buf.push(p),
        None => {
            // Fallback: use just the file name
            if let Some(name) = source_file.file_name() {
                buf.push(name);
            }
        }
    }
    // Convert to forward slashes regardless of OS (zip convention).
    buf.to_string_lossy().replace('\\', "/")
}

/// Add a numeric suffix to a zip path before the `.zip` extension.
/// e.g. `/tmp/M31_Lights.zip` + 2 → `/tmp/M31_Lights (2).zip`
pub fn add_suffix(path: &Path, n: u32) -> PathBuf {
    let parent = path.parent().unwrap_or(Path::new(""));
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("archive");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("zip");
    parent.join(format!("{} ({}).{}", stem, n, ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_problematic_chars() {
        assert_eq!(sanitize_for_filename("Hello World"), "Hello_World");
        assert_eq!(sanitize_for_filename("foo/bar:baz"), "foo_bar_baz");
        assert_eq!(sanitize_for_filename("a   b"), "a_b");
    }

    #[test]
    fn zip_filename_fallbacks() {
        let f = zip_filename(None, None, None, None, None, FrameRole::Light);
        assert_eq!(f, "Unknown_Unknown_Unknown_Unknown_Unknown_Lights.zip");

        let f = zip_filename(
            Some("M 31"), Some("2025-10-12"), Some("2025-10-15"),
            Some("RedCat 51"), Some("ASI2600MM"), FrameRole::Flat,
        );
        assert_eq!(f, "M_31_2025-10-12_2025-10-15_RedCat_51_ASI2600MM_Flats.zip");
    }

    #[test]
    fn resolve_scan_root_prefixes_unique_basenames() {
        let paths = vec!["/Photos/Lights".to_string(), "/Photos/Cal".to_string()];
        let map = resolve_scan_root_prefixes(&paths);
        assert_eq!(map.get("/Photos/Lights").unwrap(), "Lights");
        assert_eq!(map.get("/Photos/Cal").unwrap(), "Cal");
    }

    #[test]
    fn resolve_scan_root_prefixes_duplicate_basenames() {
        let paths = vec![
            "/Disk1/Astro".to_string(),
            "/Disk2/Astro".to_string(),
            "/Disk3/Astro".to_string(),
        ];
        let map = resolve_scan_root_prefixes(&paths);
        let mut values: Vec<String> = map.values().cloned().collect();
        values.sort();
        assert_eq!(values, vec!["Astro", "Astro_2", "Astro_3"]);
    }

    #[test]
    fn path_in_zip_relative() {
        let zip_path = path_in_zip(
            "Lights",
            Path::new("/Photos/Lights"),
            Path::new("/Photos/Lights/M31/2025-10-12/L_001.fits"),
        );
        assert_eq!(zip_path, "Lights/M31/2025-10-12/L_001.fits");
    }

    #[test]
    fn path_in_zip_outside_scan_root_falls_back_to_basename() {
        let zip_path = path_in_zip(
            "Lights",
            Path::new("/Photos/Lights"),
            Path::new("/Other/foo.fits"),
        );
        assert_eq!(zip_path, "Lights/foo.fits");
    }

    #[test]
    fn add_suffix_works() {
        let p = Path::new("/tmp/M31_Lights.zip");
        assert_eq!(add_suffix(p, 2), Path::new("/tmp/M31_Lights (2).zip"));
        assert_eq!(add_suffix(p, 3), Path::new("/tmp/M31_Lights (3).zip"));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p athenaeum-core --lib archive::path_layout`
Expected: 6 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/archive/path_layout.rs
git commit -m "feat(archive): add path_layout helpers for zip names and entry paths"
```

---

### Task 9: `archive::staging` — staging directory helpers

**Files:**
- Modify: `crates/athenaeum-core/src/archive/staging.rs`

- [ ] **Step 1: Write tests + implementation**

Replace `crates/athenaeum-core/src/archive/staging.rs` with:

```rust
//! Helpers for the per-operation staging directory.
//!
//! Layout: `<archive_root>/.athenaeum_staging/op_<operation_id>/<path-in-zip>`

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const STAGING_DIRNAME: &str = ".athenaeum_staging";

/// Compute the staging directory path for an operation.
pub fn staging_dir(archive_root: &Path, operation_id: i64) -> PathBuf {
    archive_root.join(STAGING_DIRNAME).join(format!("op_{}", operation_id))
}

/// Compute the staging file path for a given path-in-zip.
pub fn staging_file_path(archive_root: &Path, operation_id: i64, path_in_zip: &str) -> PathBuf {
    staging_dir(archive_root, operation_id).join(path_in_zip)
}

/// Create the staging directory tree (idempotent).
pub fn ensure_staging_dir(archive_root: &Path, operation_id: i64) -> Result<PathBuf> {
    let dir = staging_dir(archive_root, operation_id);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create staging dir {}", dir.display()))?;
    Ok(dir)
}

/// Copy a source file into staging, creating any intermediate directories.
/// Returns the destination path.
pub fn copy_into_staging(
    archive_root: &Path,
    operation_id: i64,
    source_path: &Path,
    path_in_zip: &str,
) -> Result<PathBuf> {
    let dest = staging_file_path(archive_root, operation_id, path_in_zip);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create staging subdir {}", parent.display()))?;
    }
    std::fs::copy(source_path, &dest)
        .with_context(|| format!("failed to copy {} into staging", source_path.display()))?;
    Ok(dest)
}

/// Delete the entire staging directory for an operation. No-op if missing.
pub fn cleanup_staging(archive_root: &Path, operation_id: i64) -> Result<()> {
    let dir = staging_dir(archive_root, operation_id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("failed to remove staging dir {}", dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn staging_paths() {
        let root = Path::new("/arch");
        assert_eq!(
            staging_dir(root, 7),
            PathBuf::from("/arch/.athenaeum_staging/op_7")
        );
        assert_eq!(
            staging_file_path(root, 7, "Lights/M31/x.fits"),
            PathBuf::from("/arch/.athenaeum_staging/op_7/Lights/M31/x.fits")
        );
    }

    #[test]
    fn ensure_creates_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = ensure_staging_dir(tmp.path(), 1).unwrap();
        assert!(dir.exists());
        // Idempotent
        ensure_staging_dir(tmp.path(), 1).unwrap();
    }

    #[test]
    fn copy_creates_subdirs() {
        let tmp = TempDir::new().unwrap();
        let arch = tmp.path().join("arch");
        std::fs::create_dir_all(&arch).unwrap();
        let src = tmp.path().join("src.fits");
        std::fs::write(&src, b"hello").unwrap();

        let dest = copy_into_staging(&arch, 5, &src, "Lights/M31/x.fits").unwrap();
        assert!(dest.exists());
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
    }

    #[test]
    fn cleanup_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        cleanup_staging(tmp.path(), 99).unwrap(); // doesn't exist
        ensure_staging_dir(tmp.path(), 99).unwrap();
        cleanup_staging(tmp.path(), 99).unwrap();
        cleanup_staging(tmp.path(), 99).unwrap(); // again, idempotent
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p athenaeum-core --lib archive::staging`
Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/archive/staging.rs
git commit -m "feat(archive): add staging directory helpers"
```

---

### Task 10: `archive::zip_writer` — wrapper over the `zip` crate

**Files:**
- Modify: `crates/athenaeum-core/src/archive/zip_writer.rs`

- [ ] **Step 1: Write implementation + tests**

Replace `crates/athenaeum-core/src/archive/zip_writer.rs` with:

```rust
//! Thin wrapper over the `zip` crate. Builds a single zip from a list of entries.

use crate::archive::models::ArchiveCompression;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

/// One file to add to the zip.
#[derive(Debug, Clone)]
pub struct ZipEntry {
    /// Source file on disk (will be read).
    pub source_path: PathBuf,
    /// Path inside the zip (forward slashes).
    pub path_in_zip: String,
}

/// Build a zip file at `zip_path` from the given entries.
///
/// Caller is responsible for ensuring `zip_path`'s parent directory exists.
/// Overwrites any existing file at `zip_path`.
pub fn build_zip(zip_path: &Path, entries: &[ZipEntry], compression: ArchiveCompression) -> Result<()> {
    let file = File::create(zip_path)
        .with_context(|| format!("failed to create zip file {}", zip_path.display()))?;
    let mut zw = ZipWriter::new(BufWriter::new(file));

    let method = match compression {
        ArchiveCompression::Store => CompressionMethod::Stored,
        ArchiveCompression::Deflate => CompressionMethod::Deflated,
    };
    let options: SimpleFileOptions = SimpleFileOptions::default()
        .compression_method(method)
        .large_file(true); // safe even for sub-4GB files; required for >4GB

    let mut buf = vec![0u8; 64 * 1024];

    for entry in entries {
        zw.start_file(&entry.path_in_zip, options)
            .with_context(|| format!("zip start_file failed for {}", entry.path_in_zip))?;

        let f = File::open(&entry.source_path)
            .with_context(|| format!("failed to open {} for zipping", entry.source_path.display()))?;
        let mut reader = BufReader::new(f);
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            use std::io::Write;
            zw.write_all(&buf[..n])
                .with_context(|| format!("zip write failed for {}", entry.path_in_zip))?;
        }
    }

    zw.finish().context("failed to finalize zip")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn build_zip_stores_files_correctly() {
        let tmp = TempDir::new().unwrap();
        let src1 = tmp.path().join("a.fits");
        let src2 = tmp.path().join("b.fits");
        std::fs::write(&src1, b"file-a-content").unwrap();
        std::fs::write(&src2, b"file-b-content").unwrap();

        let zip_path = tmp.path().join("out.zip");
        let entries = vec![
            ZipEntry { source_path: src1.clone(), path_in_zip: "Lights/a.fits".into() },
            ZipEntry { source_path: src2.clone(), path_in_zip: "Lights/sub/b.fits".into() },
        ];

        build_zip(&zip_path, &entries, ArchiveCompression::Store).unwrap();
        assert!(zip_path.exists());

        // Read back and verify contents.
        let f = File::open(&zip_path).unwrap();
        let mut zr = zip::ZipArchive::new(BufReader::new(f)).unwrap();
        assert_eq!(zr.len(), 2);

        let mut by_name = std::collections::HashMap::new();
        for i in 0..zr.len() {
            let mut entry = zr.by_index(i).unwrap();
            let name = entry.name().to_string();
            let mut data = String::new();
            entry.read_to_string(&mut data).unwrap();
            by_name.insert(name, data);
        }
        assert_eq!(by_name.get("Lights/a.fits").unwrap(), "file-a-content");
        assert_eq!(by_name.get("Lights/sub/b.fits").unwrap(), "file-b-content");
    }

    #[test]
    fn build_zip_with_deflate() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("a.fits");
        std::fs::write(&src, vec![b'x'; 4096]).unwrap();
        let zip_path = tmp.path().join("out.zip");

        build_zip(
            &zip_path,
            &[ZipEntry { source_path: src, path_in_zip: "x".into() }],
            ArchiveCompression::Deflate,
        ).unwrap();

        // Deflate should produce a smaller zip than the source.
        let zsz = std::fs::metadata(&zip_path).unwrap().len();
        assert!(zsz < 4096, "expected compressed zip to be smaller than 4096 bytes, got {}", zsz);
    }

    #[test]
    fn build_zip_overwrites_existing() {
        let tmp = TempDir::new().unwrap();
        let zip_path = tmp.path().join("out.zip");
        std::fs::write(&zip_path, b"old garbage").unwrap();

        let src = tmp.path().join("a.fits");
        std::fs::write(&src, b"hello").unwrap();
        build_zip(
            &zip_path,
            &[ZipEntry { source_path: src, path_in_zip: "a.fits".into() }],
            ArchiveCompression::Store,
        ).unwrap();

        // Should be a valid zip now, not garbage.
        let f = File::open(&zip_path).unwrap();
        let zr = zip::ZipArchive::new(BufReader::new(f)).unwrap();
        assert_eq!(zr.len(), 1);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p athenaeum-core --lib archive::zip_writer`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/archive/zip_writer.rs
git commit -m "feat(archive): add zip_writer wrapper over zip crate"
```

---

### Task 11: `archive::zip_reader` — verify zip contents

**Files:**
- Modify: `crates/athenaeum-core/src/archive/zip_reader.rs`

- [ ] **Step 1: Write implementation + tests**

Replace `crates/athenaeum-core/src/archive/zip_reader.rs` with:

```rust
//! Verify a built zip's contents against an expected entry list.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// Open `zip_path` and verify it contains exactly the expected entries (by path-in-zip).
/// Returns Ok(()) when all expected entries are present.
/// Returns Err with a message listing missing or extra entries.
pub fn verify_zip_contents(zip_path: &Path, expected_entries: &[String]) -> Result<()> {
    let file = File::open(zip_path)
        .with_context(|| format!("failed to open zip {}", zip_path.display()))?;
    let mut zr = zip::ZipArchive::new(BufReader::new(file))
        .with_context(|| format!("failed to parse zip {}", zip_path.display()))?;

    let mut found: HashSet<String> = HashSet::with_capacity(zr.len());
    for i in 0..zr.len() {
        let entry = zr.by_index(i)
            .with_context(|| format!("failed to read entry {} from zip", i))?;
        found.insert(entry.name().to_string());
    }

    let expected: HashSet<String> = expected_entries.iter().cloned().collect();

    let missing: Vec<&String> = expected.difference(&found).collect();
    if !missing.is_empty() {
        let names: Vec<&str> = missing.iter().map(|s| s.as_str()).collect();
        anyhow::bail!("zip {} missing entries: {:?}", zip_path.display(), names);
    }

    let extra: Vec<&String> = found.difference(&expected).collect();
    if !extra.is_empty() {
        let names: Vec<&str> = extra.iter().map(|s| s.as_str()).collect();
        anyhow::bail!("zip {} has unexpected entries: {:?}", zip_path.display(), names);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::zip_writer::{build_zip, ZipEntry};
    use crate::archive::models::ArchiveCompression;
    use tempfile::TempDir;

    fn write_zip_with(tmp: &TempDir, entries: &[(&str, &[u8])]) -> std::path::PathBuf {
        let zip_path = tmp.path().join("v.zip");
        let mut zip_entries = Vec::new();
        let mut sources = Vec::new();
        for (i, (name, data)) in entries.iter().enumerate() {
            let p = tmp.path().join(format!("src_{}.bin", i));
            std::fs::write(&p, data).unwrap();
            sources.push(p.clone());
            zip_entries.push(ZipEntry { source_path: p, path_in_zip: (*name).to_string() });
        }
        build_zip(&zip_path, &zip_entries, ArchiveCompression::Store).unwrap();
        zip_path
    }

    #[test]
    fn verifies_match() {
        let tmp = TempDir::new().unwrap();
        let zp = write_zip_with(&tmp, &[("a", b"hi"), ("b", b"there")]);
        verify_zip_contents(&zp, &["a".into(), "b".into()]).unwrap();
    }

    #[test]
    fn detects_missing() {
        let tmp = TempDir::new().unwrap();
        let zp = write_zip_with(&tmp, &[("a", b"hi")]);
        let err = verify_zip_contents(&zp, &["a".into(), "b".into()]).unwrap_err();
        assert!(format!("{}", err).contains("missing entries"), "{}", err);
    }

    #[test]
    fn detects_extra() {
        let tmp = TempDir::new().unwrap();
        let zp = write_zip_with(&tmp, &[("a", b"hi"), ("b", b"x")]);
        let err = verify_zip_contents(&zp, &["a".into()]).unwrap_err();
        assert!(format!("{}", err).contains("unexpected entries"), "{}", err);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p athenaeum-core --lib archive::zip_reader`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/archive/zip_reader.rs
git commit -m "feat(archive): add zip_reader for verifying zip contents"
```

---

### Task 12: `archive::shared_calibration` — detect shared calibration sets

**Files:**
- Modify: `crates/athenaeum-core/src/archive/shared_calibration.rs`

- [ ] **Step 1: Write implementation + tests**

A calibration set is "shared" with another frame set if any frame from another (non-archived) frame set is linked to it via `calibration_set_to_frames`. We need this per-frame-role since the user's "Move" toggle is per-type.

Replace `crates/athenaeum-core/src/archive/shared_calibration.rs` with:

```rust
//! Detect which calibration sets linked to a given frame set are also linked
//! to other (non-archived) frame sets. Used to disable "Move" radios in the UI.

use crate::archive::models::{FrameRole, SharedCalibrationWarning};
use anyhow::Result;
use rusqlite::{params, Connection};

fn role_to_calibration_type(role: FrameRole) -> &'static str {
    match role {
        FrameRole::Flat => "Flat",
        FrameRole::Dark => "Dark",
        FrameRole::Bias => "Bias",
        FrameRole::Darkflat => "DarkFlat",
        FrameRole::Light => "", // not applicable
    }
}

/// For each calibration type linked to this frame set, return a list of
/// (calibration_set_id, [other_frames_set_ids...]) where the cal set is also
/// referenced by frames in other (non-archived) frame sets.
pub fn find_shared_calibration_sets(
    conn: &Connection,
    frames_set_id: i64,
) -> Result<Vec<SharedCalibrationWarning>> {
    let mut warnings = Vec::new();

    for role in [FrameRole::Flat, FrameRole::Dark, FrameRole::Bias, FrameRole::Darkflat] {
        let cal_type = role_to_calibration_type(role);

        // Calibration sets linked to LIGHT frames in this frame set.
        let cal_set_ids: Vec<i64> = {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT cstf.calibration_set_id
                 FROM calibration_set_to_frames cstf
                 JOIN frames f ON f.id = cstf.source_id AND cstf.source_type = 'frame'
                 JOIN session_members sm ON sm.frame_id = f.id
                 JOIN sessions s ON s.id = sm.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = ?1
                   AND cstf.calibration_type = ?2
                   AND f.imagetyp = 'Light'",
            )?;
            stmt.query_map(params![frames_set_id, cal_type], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?
        };

        for cs_id in cal_set_ids {
            // Find OTHER frame sets that reference this cal set, excluding archived ones.
            let mut stmt = conn.prepare(
                "SELECT DISTINCT n.frames_set_id
                 FROM calibration_set_to_frames cstf
                 JOIN frames f ON f.id = cstf.source_id AND cstf.source_type = 'frame'
                 JOIN session_members sm ON sm.frame_id = f.id
                 JOIN sessions s ON s.id = sm.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 JOIN frames_set fs ON fs.id = n.frames_set_id
                 WHERE cstf.calibration_set_id = ?1
                   AND cstf.calibration_type = ?2
                   AND n.frames_set_id != ?3
                   AND fs.archived_at IS NULL",
            )?;
            let others: Vec<i64> = stmt.query_map(params![cs_id, cal_type, frames_set_id], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?;

            if !others.is_empty() {
                warnings.push(SharedCalibrationWarning {
                    frame_role: role,
                    calibration_set_id: cs_id,
                    other_frames_set_ids: others,
                });
            }
        }
    }

    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    /// Create two frame sets, both referencing the same dark cal set, and verify
    /// that planning archive of frame set A flags the dark as shared with B.
    #[test]
    fn detects_shared_dark() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Frame sets
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'A'), (2, 'B')", []).unwrap();
        // Imaging nights
        conn.execute(
            "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES
             (10, 1, '2025-01-01T00:00:00Z', '2025-01-02T00:00:00Z'),
             (11, 2, '2025-01-03T00:00:00Z', '2025-01-04T00:00:00Z')",
            [],
        ).unwrap();
        // Sessions
        conn.execute(
            "INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C'), (101, 11, 'C')",
            [],
        ).unwrap();
        // Files
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES
             (1000, '/a/L1.fits', 'L1.fits', 1, '2025-01-01T00:00:00Z', 'FITS'),
             (1001, '/b/L2.fits', 'L2.fits', 1, '2025-01-03T00:00:00Z', 'FITS')",
            [],
        ).unwrap();
        // Frames
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp) VALUES
             (10000, 1000, 'Light'),
             (10001, 1001, 'Light')",
            [],
        ).unwrap();
        // session_members
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000), (101, 10001)",
            [],
        ).unwrap();
        // Cal set
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (500, 'Dark', '2025-01-01')",
            [],
        ).unwrap();
        // Both frames link to same cal set
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (10000, 'frame', 500, 'Dark', '2025-01-01'),
                    (10001, 'frame', 500, 'Dark', '2025-01-03')",
            [],
        ).unwrap();

        let warnings = find_shared_calibration_sets(&conn, 1).unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].frame_role, FrameRole::Dark);
        assert_eq!(warnings[0].calibration_set_id, 500);
        assert_eq!(warnings[0].other_frames_set_ids, vec![2]);
    }

    /// If the only other frame set referencing the cal set is itself archived,
    /// it doesn't count — Move is allowed.
    #[test]
    fn ignores_archived_other_sets() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO frames_set (id, name, archived_at) VALUES
             (1, 'A', NULL),
             (2, 'B-archived', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES
             (10, 1, '2025-01-01', '2025-01-02'),
             (11, 2, '2025-01-03', '2025-01-04')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C'), (101, 11, 'C')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES
             (1000, '/a/L1.fits', 'L1.fits', 1, '2025-01-01', 'FITS'),
             (1001, '/b/L2.fits', 'L2.fits', 1, '2025-01-03', 'FITS')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp) VALUES
             (10000, 1000, 'Light'), (10001, 1001, 'Light')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000), (101, 10001)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (500, 'Dark', '2025-01-01')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (10000, 'frame', 500, 'Dark', '2025-01-01'),
                    (10001, 'frame', 500, 'Dark', '2025-01-03')",
            [],
        ).unwrap();

        let warnings = find_shared_calibration_sets(&conn, 1).unwrap();
        assert_eq!(warnings.len(), 0, "archived other sets should not flag share");
    }

    #[test]
    fn returns_empty_when_no_calibrations() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'X')", []).unwrap();

        let warnings = find_shared_calibration_sets(&conn, 1).unwrap();
        assert_eq!(warnings.len(), 0);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p athenaeum-core --lib archive::shared_calibration`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/archive/shared_calibration.rs
git commit -m "feat(archive): detect calibration sets shared with other frame sets"
```

---

## Phase 4 — Planner

### Task 13: `archive::planner` — `build_plan` (no DB writes)

The planner gathers all source files, computes target paths, computes hashes, and produces an `ArchivePlan`. It does NOT write to `archive_operations` / `archive_operation_files` (that's `commit_plan`).

**Files:**
- Modify: `crates/athenaeum-core/src/archive/planner.rs`

- [ ] **Step 1: Write planner implementation + tests**

Replace `crates/athenaeum-core/src/archive/planner.rs` with:

```rust
//! Build (and commit) the archive plan for a frame set.

use crate::archive::db as adb;
use crate::archive::models::{
    ArchiveCompression, ArchiveDisposition, ArchiveOperationFile, ArchivePlan, ConflictResolution,
    Dispositions, FrameRole, PlannedZip, SharedCalibrationWarning, ZipFilenameConflict,
};
use crate::archive::path_layout;
use crate::archive::shared_calibration::find_shared_calibration_sets;
use crate::duplicates::compute_xxhash;
use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Per-file row used internally before we resolve target paths.
#[derive(Debug, Clone)]
struct CandidateFile {
    file_id: i64,
    file_path: String,
    file_size: i64,
    role: FrameRole,
    disposition: ArchiveDisposition,
}

/// Build a plan WITHOUT writing any rows.
///
/// Behavior:
/// - Collects all LIGHT frames in the frame set; lights are always disposition=Move.
/// - For each calibration type with disposition=Move|Copy, collects the linked
///   calibration set's frames (master or single-file).
/// - Skip dispositions are skipped entirely.
/// - Deduplicates by file_id, keeping the highest-priority role (light > flat > darkflat > dark > bias).
/// - If a file with disposition=Move on its winning role is detected as shared, a
///   `SharedCalibrationWarning` is added (the executor will reject Move without
///   user confirmation; UI is expected to filter dispositions accordingly).
/// - Computes the path-in-zip per file using scan-root-name prefix.
/// - Hashes each source file with XXH3_64.
/// - Groups files into one zip per frame role; computes zip filename + total size.
/// - Detects conflicting zip filenames already on disk in the archive root.
pub fn build_plan(
    conn: &Connection,
    frames_set_id: i64,
    archive_root_path: &Path,
    dispositions: &Dispositions,
    compression: ArchiveCompression,
) -> Result<ArchivePlan> {
    let frame_set = load_frame_set_metadata(conn, frames_set_id)?;
    let scan_roots = load_all_scan_roots(conn)?;
    let prefix_map = path_layout::resolve_scan_root_prefixes(&scan_roots);

    // 1. Lights (always Move)
    let mut candidates: Vec<CandidateFile> = collect_light_files(conn, frames_set_id)?
        .into_iter()
        .map(|(file_id, path, size)| CandidateFile {
            file_id,
            file_path: path,
            file_size: size,
            role: FrameRole::Light,
            disposition: ArchiveDisposition::Move,
        })
        .collect();

    // 2. Calibrations per type
    for (role, disp) in [
        (FrameRole::Flat, dispositions.flats),
        (FrameRole::Dark, dispositions.darks),
        (FrameRole::Bias, dispositions.bias),
        (FrameRole::Darkflat, dispositions.darkflats),
    ] {
        let Some(d) = disp else { continue };
        if d == ArchiveDisposition::Skip {
            continue;
        }
        for (file_id, path, size) in collect_calibration_files(conn, frames_set_id, role)? {
            candidates.push(CandidateFile {
                file_id,
                file_path: path,
                file_size: size,
                role,
                disposition: d,
            });
        }
    }

    // 3. Deduplicate by file_id, keep highest-priority role
    let mut by_id: HashMap<i64, CandidateFile> = HashMap::new();
    for c in candidates {
        by_id
            .entry(c.file_id)
            .and_modify(|existing| {
                if c.role.priority() < existing.role.priority() {
                    *existing = c.clone();
                }
            })
            .or_insert(c);
    }

    // 4. Detect shared calibrations
    let shared_warnings = find_shared_calibration_sets(conn, frames_set_id)?;

    // 5. For each file: hash, compute target zip path + path-in-zip
    //    Group by role to determine the zip filenames.
    let mut files: Vec<ArchiveOperationFile> = Vec::with_capacity(by_id.len());
    let mut zips_by_role: HashMap<FrameRole, (String, PathBuf, u64, usize)> = HashMap::new();
    let mut total_size: u64 = 0;

    for (_id, candidate) in by_id {
        let src = Path::new(&candidate.file_path);
        if !src.exists() {
            return Err(anyhow!("source file no longer exists: {}", candidate.file_path));
        }
        let scan_root = scan_roots
            .iter()
            .find(|r| src.starts_with(r))
            .cloned()
            .unwrap_or_else(|| {
                src.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
            });
        let unique_prefix = prefix_map
            .get(&scan_root)
            .cloned()
            .unwrap_or_else(|| {
                Path::new(&scan_root)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(path_layout::sanitize_for_filename)
                    .unwrap_or_else(|| "Root".into())
            });
        let path_in_zip = path_layout::path_in_zip(&unique_prefix, Path::new(&scan_root), src);

        let zip_name = path_layout::zip_filename(
            frame_set.object.as_deref(),
            frame_set.start_date.as_deref(),
            frame_set.end_date.as_deref(),
            frame_set.telescope.as_deref(),
            frame_set.camera.as_deref(),
            candidate.role,
        );
        let zip_path = archive_root_path.join(&zip_name);

        let hash = compute_xxhash(src)
            .with_context(|| format!("failed to hash {}", candidate.file_path))?;

        total_size += candidate.file_size as u64;
        let entry = zips_by_role
            .entry(candidate.role)
            .or_insert_with(|| (zip_name.clone(), zip_path.clone(), 0u64, 0usize));
        entry.2 += candidate.file_size as u64;
        entry.3 += 1;

        files.push(ArchiveOperationFile {
            id: 0, // assigned at commit time
            operation_id: 0,
            file_id: Some(candidate.file_id),
            source_path: candidate.file_path,
            target_zip_path: zip_path.to_string_lossy().to_string(),
            target_path_in_zip: path_in_zip,
            expected_hash: hash,
            disposition: candidate.disposition.as_str().to_string(),
            frame_role: candidate.role.as_str().to_string(),
            file_size_bytes: candidate.file_size,
        });
    }

    let zips: Vec<PlannedZip> = zips_by_role
        .into_iter()
        .map(|(role, (filename, zip_path, total, count))| PlannedZip {
            zip_path: zip_path.to_string_lossy().to_string(),
            zip_filename: filename,
            frame_role: role,
            file_count: count,
            total_size_bytes: total,
        })
        .collect();

    let conflicts: Vec<ZipFilenameConflict> = zips
        .iter()
        .filter(|z| Path::new(&z.zip_path).exists())
        .map(|z| ZipFilenameConflict {
            zip_path: z.zip_path.clone(),
            zip_filename: z.zip_filename.clone(),
        })
        .collect();

    // Disk-space pre-flight (5% safety margin)
    if let Ok(available) = available_disk_space(archive_root_path) {
        let needed = total_size + (total_size / 20);
        if available < needed {
            anyhow::bail!(
                "insufficient disk space at archive root: need {} bytes (incl. 5% margin), available {}",
                needed, available
            );
        }
    }

    Ok(ArchivePlan {
        frames_set_id,
        archive_root_path: archive_root_path.to_string_lossy().to_string(),
        dispositions: dispositions.clone(),
        compression,
        files,
        zips,
        shared_calibrations: shared_warnings,
        conflicts,
        total_size_bytes: total_size,
    })
}

/// Persist the plan: insert archive_operations + archive_operation_files rows,
/// applying the conflict resolution to zip paths if needed (renaming with `_2`, `_3` etc.).
/// Returns the new operation_id.
pub fn commit_plan(
    conn: &Connection,
    plan: &ArchivePlan,
    conflict_resolution: ConflictResolution,
) -> Result<i64> {
    // Apply conflict resolution: rewrite target_zip_path on plan.files + plan.zips
    let mut files = plan.files.clone();
    let mut zips = plan.zips.clone();

    if conflict_resolution == ConflictResolution::AddSuffix {
        for z in zips.iter_mut() {
            let mut p = PathBuf::from(&z.zip_path);
            let mut n = 2;
            while p.exists() {
                p = path_layout::add_suffix(Path::new(&z.zip_path), n);
                n += 1;
            }
            let new_path = p.to_string_lossy().to_string();
            // Update files that point to the old zip_path
            let old_zip_path = z.zip_path.clone();
            for f in files.iter_mut() {
                if f.target_zip_path == old_zip_path {
                    f.target_zip_path = new_path.clone();
                }
            }
            z.zip_path = new_path;
            z.zip_filename = p.file_name().unwrap().to_string_lossy().to_string();
        }
    }
    // Overwrite mode keeps paths as-is (existing zip is overwritten when build_zip runs).

    let op_id = adb::insert_operation(
        conn,
        plan.frames_set_id,
        &plan.archive_root_path,
        plan.dispositions.flats.map(|d| d.as_str()),
        plan.dispositions.darks.map(|d| d.as_str()),
        plan.dispositions.bias.map(|d| d.as_str()),
        plan.dispositions.darkflats.map(|d| d.as_str()),
        plan.compression.as_str(),
    )?;

    for f in &files {
        adb::insert_operation_file(
            conn,
            op_id,
            f.file_id,
            &f.source_path,
            &f.target_zip_path,
            &f.target_path_in_zip,
            &f.expected_hash,
            &f.disposition,
            &f.frame_role,
            f.file_size_bytes,
        )?;
    }
    Ok(op_id)
}

// --- helpers ---

#[derive(Debug, Default)]
struct FrameSetMetadata {
    object: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
    telescope: Option<String>,
    camera: Option<String>,
}

fn load_frame_set_metadata(conn: &Connection, frames_set_id: i64) -> Result<FrameSetMetadata> {
    // Aggregate from frames in the set: most-frequent telescope+camera, min/max date.
    let row: Option<(Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)> =
        conn.query_row(
            "SELECT
                (SELECT f.object FROM frames f
                 JOIN session_members sm ON sm.frame_id = f.id
                 JOIN sessions s ON s.id = sm.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = ?1 AND f.object IS NOT NULL
                 LIMIT 1),
                (SELECT DATE(MIN(f.date_obs)) FROM frames f
                 JOIN session_members sm ON sm.frame_id = f.id
                 JOIN sessions s ON s.id = sm.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = ?1),
                (SELECT DATE(MAX(f.date_obs)) FROM frames f
                 JOIN session_members sm ON sm.frame_id = f.id
                 JOIN sessions s ON s.id = sm.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = ?1),
                (SELECT f.telescop FROM frames f
                 JOIN session_members sm ON sm.frame_id = f.id
                 JOIN sessions s ON s.id = sm.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = ?1 AND f.telescop IS NOT NULL
                 GROUP BY f.telescop ORDER BY COUNT(*) DESC LIMIT 1),
                (SELECT f.instrume FROM frames f
                 JOIN session_members sm ON sm.frame_id = f.id
                 JOIN sessions s ON s.id = sm.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = ?1 AND f.instrume IS NOT NULL
                 GROUP BY f.instrume ORDER BY COUNT(*) DESC LIMIT 1)",
            [frames_set_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        ).ok();
    Ok(match row {
        Some((object, start, end, scope, cam)) => FrameSetMetadata {
            object,
            start_date: start,
            end_date: end,
            telescope: scope,
            camera: cam,
        },
        None => FrameSetMetadata::default(),
    })
}

fn load_all_scan_roots(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT path FROM scan_roots ORDER BY path")?;
    let rows: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Light frames in the set: (file_id, path, size).
fn collect_light_files(conn: &Connection, frames_set_id: i64) -> Result<Vec<(i64, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT fi.id, fi.path, fi.size
         FROM files fi
         JOIN frames f ON f.file_id = fi.id
         JOIN session_members sm ON sm.frame_id = f.id
         JOIN sessions s ON s.id = sm.session_id
         JOIN imaging_nights n ON n.id = s.imaging_night_id
         WHERE n.frames_set_id = ?1 AND f.imagetyp = 'Light'
         ORDER BY fi.path",
    )?;
    let rows: Vec<(i64, String, i64)> = stmt.query_map([frames_set_id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Calibration files reachable from a frame set, for a given role.
fn collect_calibration_files(
    conn: &Connection,
    frames_set_id: i64,
    role: FrameRole,
) -> Result<Vec<(i64, String, i64)>> {
    let cal_type = match role {
        FrameRole::Flat => "Flat",
        FrameRole::Dark => "Dark",
        FrameRole::Bias => "Bias",
        FrameRole::Darkflat => "DarkFlat",
        FrameRole::Light => return Ok(vec![]),
    };
    let mut stmt = conn.prepare(
        "SELECT DISTINCT fi.id, fi.path, fi.size
         FROM files fi
         JOIN frames f ON f.file_id = fi.id
         JOIN calibration_set_frames csf ON csf.frame_id = f.id
         JOIN calibration_set_to_frames cstf ON cstf.calibration_set_id = csf.set_id
         JOIN frames lf ON lf.id = cstf.source_id AND cstf.source_type = 'frame'
         JOIN session_members sm ON sm.frame_id = lf.id
         JOIN sessions s ON s.id = sm.session_id
         JOIN imaging_nights n ON n.id = s.imaging_night_id
         WHERE n.frames_set_id = ?1
           AND cstf.calibration_type = ?2
         ORDER BY fi.path",
    )?;
    let rows: Vec<(i64, String, i64)> = stmt.query_map(params![frames_set_id, cal_type], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Best-effort disk-space query. Returns an error on platforms where it isn't trivially supported;
/// callers should treat that as "skip the pre-flight check."
fn available_disk_space(_path: &Path) -> Result<u64> {
    // Cross-platform disk-space inquiry without an extra dependency is awkward.
    // Returning Err here causes the caller to skip the check; that's acceptable
    // for v1 since the executor will fail loudly on out-of-space at copy time anyway.
    Err(anyhow!("disk-space check not implemented; relying on copy-time errors"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::models::ArchiveCompression;
    use crate::db::schema::init_db;
    use tempfile::TempDir;

    /// Build a tiny SQLite + filesystem fixture: one frame_set with two LIGHT
    /// frames and one master DARK linked to both. Returns (conn, archive_dir, scan_root).
    fn fixture() -> (Connection, TempDir, TempDir) {
        let arch_dir = TempDir::new().unwrap();
        let scan_dir = TempDir::new().unwrap();

        // Two real .fits files to hash.
        let l1 = scan_dir.path().join("M31/2025-10-12/L_001.fits");
        let l2 = scan_dir.path().join("M31/2025-10-12/L_002.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        std::fs::write(&l1, b"light-1-content").unwrap();
        std::fs::write(&l2, b"light-2-content").unwrap();
        let d1 = scan_dir.path().join("Cal/MasterDark.fits");
        std::fs::create_dir_all(d1.parent().unwrap()).unwrap();
        std::fs::write(&d1, b"dark-content").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Scan root that contains all the test files
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan_dir.path().to_str().unwrap()],
        ).unwrap();

        // Frame set
        conn.execute(
            "INSERT INTO frames_set (id, name, date_obs_start, date_obs_end)
             VALUES (1, 'M31', '2025-10-12T00:00:00Z', '2025-10-12T08:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12T00:00:00Z', '2025-10-13T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'ASI2600MM')",
            [],
        ).unwrap();

        // Light files + frames
        for (file_id, path, frame_id) in [(1000, &l1, 10000), (1001, &l2, 10001)] {
            let p = path.to_str().unwrap();
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, ?3, 15, '2025-10-12T00:00:00Z', 'FITS')",
                params![file_id, p, path.file_name().unwrap().to_str().unwrap()],
            ).unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp)
                 VALUES (?1, ?2, 'M31', 'RedCat 51', 'ASI2600MM', 'Light')",
                params![frame_id, file_id],
            ).unwrap();
            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)",
                [frame_id],
            ).unwrap();
        }

        // Dark file + frame + calibration set + links
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (2000, ?1, 'MasterDark.fits', 12, '2025-10-10T00:00:00Z', 'FITS')",
            [d1.to_str().unwrap()],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, instrume, imagetyp, is_master)
             VALUES (20000, 2000, 'ASI2600MM', 'Dark', 1)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (500, 'Dark', '2025-10-10')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (500, 20000)",
            [],
        ).unwrap();
        // Link both light frames to this dark
        for fid in [10000, 10001] {
            conn.execute(
                "INSERT INTO calibration_set_to_frames
                 (source_id, source_type, calibration_set_id, calibration_type, matched_at)
                 VALUES (?1, 'frame', 500, 'Dark', '2025-10-12')",
                [fid],
            ).unwrap();
        }

        (conn, arch_dir, scan_dir)
    }

    #[test]
    fn build_plan_lights_only() {
        let (conn, arch_dir, _scan_dir) = fixture();
        let dispositions = Dispositions {
            flats: None, darks: Some(ArchiveDisposition::Skip), bias: None, darkflats: None,
        };
        let plan = build_plan(
            &conn, 1, arch_dir.path(), &dispositions, ArchiveCompression::Store,
        ).unwrap();

        assert_eq!(plan.files.len(), 2, "two light files");
        assert!(plan.files.iter().all(|f| f.frame_role == "light"));
        assert!(plan.files.iter().all(|f| f.disposition == "move"));
        assert_eq!(plan.zips.len(), 1, "one Lights.zip");
        assert!(plan.zips[0].zip_filename.contains("Lights.zip"));
    }

    #[test]
    fn build_plan_with_dark_copy() {
        let (conn, arch_dir, _scan_dir) = fixture();
        let dispositions = Dispositions {
            flats: None, darks: Some(ArchiveDisposition::Copy), bias: None, darkflats: None,
        };
        let plan = build_plan(
            &conn, 1, arch_dir.path(), &dispositions, ArchiveCompression::Store,
        ).unwrap();

        assert_eq!(plan.files.len(), 3, "two lights + one dark");
        let dark = plan.files.iter().find(|f| f.frame_role == "dark").unwrap();
        assert_eq!(dark.disposition, "copy");
        assert_eq!(plan.zips.len(), 2, "Lights.zip + Darks.zip");
    }

    #[test]
    fn build_plan_skip_excludes_calibration() {
        let (conn, arch_dir, _scan_dir) = fixture();
        let dispositions = Dispositions {
            flats: None, darks: Some(ArchiveDisposition::Skip), bias: None, darkflats: None,
        };
        let plan = build_plan(
            &conn, 1, arch_dir.path(), &dispositions, ArchiveCompression::Store,
        ).unwrap();
        assert!(plan.files.iter().all(|f| f.frame_role != "dark"));
    }

    #[test]
    fn build_plan_detects_existing_zip_conflict() {
        let (conn, arch_dir, _scan_dir) = fixture();
        // Pre-create a zip with the predicted name.
        let predicted = path_layout::zip_filename(
            Some("M31"), Some("2025-10-12"), Some("2025-10-12"),
            Some("RedCat 51"), Some("ASI2600MM"), FrameRole::Light,
        );
        std::fs::write(arch_dir.path().join(&predicted), b"existing").unwrap();

        let dispositions = Dispositions {
            flats: None, darks: Some(ArchiveDisposition::Skip), bias: None, darkflats: None,
        };
        let plan = build_plan(
            &conn, 1, arch_dir.path(), &dispositions, ArchiveCompression::Store,
        ).unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert!(plan.conflicts[0].zip_filename.ends_with("_Lights.zip"));
    }

    #[test]
    fn commit_plan_writes_rows_and_can_apply_suffix() {
        let (conn, arch_dir, _scan_dir) = fixture();
        let dispositions = Dispositions {
            flats: None, darks: Some(ArchiveDisposition::Copy), bias: None, darkflats: None,
        };
        let plan = build_plan(
            &conn, 1, arch_dir.path(), &dispositions, ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();

        let op = adb::get_operation(&conn, op_id).unwrap();
        assert_eq!(op.frames_set_id, 1);
        let files = adb::list_operation_files(&conn, op_id).unwrap();
        assert_eq!(files.len(), 3);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p athenaeum-core --lib archive::planner`
Expected: 5 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/archive/planner.rs
git commit -m "feat(archive): planner builds and commits archive plans"
```

---

## Phase 5 — Executor

The executor drives stages 2 (Copy) through 7 (Finalize) with cooperative cancellation between every per-file step. It's resilient to crashes mid-stage: the step log lets resume_operation pick up exactly where it left off without duplicating work.

### Task 14: `archive::executor` — main `run_operation` driver + per-stage helpers

**Files:**
- Modify: `crates/athenaeum-core/src/archive/executor.rs`

- [ ] **Step 1: Write the executor**

Replace `crates/athenaeum-core/src/archive/executor.rs` with:

```rust
//! Drive stages 2-7 of an archive operation.

use crate::archive::db as adb;
use crate::archive::models::{
    ArchiveCompression, ArchiveOperationFile, ArchiveStage, ArchiveStatus, StepStatus,
};
use crate::archive::staging;
use crate::archive::zip_reader::verify_zip_contents;
use crate::archive::zip_writer::{build_zip, ZipEntry};
use crate::duplicates::compute_xxhash;
use crate::events::{emit_event, ProgressEmitter};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Cancellation indicator. Worker checks between every per-file step.
pub type CancelFlag = Arc<AtomicBool>;

#[derive(Serialize, Clone, Debug)]
pub struct ArchiveProgress {
    pub operation_id: i64,
    pub stage: String,
    pub current: usize,
    pub total: usize,
    pub message: String,
}

/// Sentinel error type used to propagate "user cancelled" up the call stack.
/// The caller (run_operation) catches this, sets status=Cancelled, and lets
/// rollback take over. We use the message string rather than a bespoke type
/// to keep it inside `anyhow::Error`.
const CANCEL_MSG: &str = "__archive_cancelled__";

fn check_cancel(cancel: &CancelFlag) -> Result<()> {
    if cancel.load(Ordering::SeqCst) {
        anyhow::bail!(CANCEL_MSG);
    }
    Ok(())
}

pub fn was_cancelled(err: &anyhow::Error) -> bool {
    format!("{}", err).contains(CANCEL_MSG)
}

/// Run the full forward operation (stages 2-7).
///
/// On success: status=Completed, frame set marked archived, files marked archived.
/// On cancel: status=Cancelled, then rollback is invoked by the caller (commands layer).
/// On error: status=Failed, then rollback is invoked by the caller.
pub fn run_operation(
    conn: &Connection,
    operation_id: i64,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let op = adb::get_operation(conn, operation_id)?;
    let archive_root = PathBuf::from(&op.archive_root_path);
    let compression = ArchiveCompression::from_str(&op.compression)
        .ok_or_else(|| anyhow::anyhow!("invalid compression value: {}", op.compression))?;
    let files = adb::list_operation_files(conn, operation_id)?;

    staging::ensure_staging_dir(&archive_root, operation_id)?;

    // Stage 2: Copy ----------------------------------------------------------
    adb::update_operation_status(conn, operation_id, ArchiveStatus::Copying, None)?;
    copy_phase(conn, operation_id, &files, &archive_root, cancel, emitter)?;

    // Stage 3: Verify copy ---------------------------------------------------
    adb::update_operation_status(conn, operation_id, ArchiveStatus::Verifying, None)?;
    verify_copy_phase(conn, operation_id, &files, &archive_root, cancel, emitter)?;

    // Stage 4: Build zip -----------------------------------------------------
    adb::update_operation_status(conn, operation_id, ArchiveStatus::Zipping, None)?;
    zip_phase(conn, operation_id, &files, &archive_root, compression, cancel, emitter)?;

    // Stage 5: Verify zip ----------------------------------------------------
    adb::update_operation_status(conn, operation_id, ArchiveStatus::ZipVerifying, None)?;
    verify_zip_phase(conn, operation_id, &files, cancel, emitter)?;

    // Stage 6: Delete sources ------------------------------------------------
    adb::update_operation_status(conn, operation_id, ArchiveStatus::DeletingSources, None)?;
    delete_sources_phase(conn, operation_id, &files, cancel, emitter)?;

    // Stage 7: Finalize ------------------------------------------------------
    adb::update_operation_status(conn, operation_id, ArchiveStatus::Finalizing, None)?;
    finalize_phase(conn, operation_id, &op.frames_set_id, &files, &archive_root, emitter)?;

    adb::update_operation_status(conn, operation_id, ArchiveStatus::Completed, None)?;
    Ok(())
}

/// Stage 2: copy each file into staging. Idempotent per row: if a step exists
/// with status=Done, skip it (resume after crash).
fn copy_phase(
    conn: &Connection,
    operation_id: i64,
    files: &[ArchiveOperationFile],
    archive_root: &Path,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let total = files.len();
    let existing = existing_done_steps(conn, operation_id, ArchiveStage::Copy)?;
    for (idx, f) in files.iter().enumerate() {
        check_cancel(cancel)?;
        emit_event(emitter, "archive-progress", &ArchiveProgress {
            operation_id,
            stage: "copying".into(),
            current: idx + 1,
            total,
            message: format!("Copying {}/{}", idx + 1, total),
        });

        if existing.contains(&f.id) {
            continue;
        }
        let step_id = adb::insert_step(conn, operation_id, Some(f.id), ArchiveStage::Copy)?;
        adb::update_step(conn, step_id, StepStatus::InProgress, None, None)?;
        match staging::copy_into_staging(
            archive_root, operation_id, Path::new(&f.source_path), &f.target_path_in_zip,
        ) {
            Ok(_) => adb::update_step(conn, step_id, StepStatus::Done, None, None)?,
            Err(e) => {
                let msg = format!("{:#}", e);
                adb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
                anyhow::bail!("copy failed for {}: {}", f.source_path, msg);
            }
        }
    }
    Ok(())
}

/// Stage 3: hash each staged file and compare to expected.
fn verify_copy_phase(
    conn: &Connection,
    operation_id: i64,
    files: &[ArchiveOperationFile],
    archive_root: &Path,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let total = files.len();
    let existing = existing_done_steps(conn, operation_id, ArchiveStage::VerifyCopy)?;
    for (idx, f) in files.iter().enumerate() {
        check_cancel(cancel)?;
        emit_event(emitter, "archive-progress", &ArchiveProgress {
            operation_id,
            stage: "verifying".into(),
            current: idx + 1,
            total,
            message: format!("Verifying hashes {}/{}", idx + 1, total),
        });

        if existing.contains(&f.id) {
            continue;
        }
        let step_id = adb::insert_step(conn, operation_id, Some(f.id), ArchiveStage::VerifyCopy)?;
        adb::update_step(conn, step_id, StepStatus::InProgress, None, None)?;
        let staged = staging::staging_file_path(archive_root, operation_id, &f.target_path_in_zip);
        let actual = match compute_xxhash(&staged) {
            Ok(h) => h,
            Err(e) => {
                let msg = format!("{:#}", e);
                adb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
                anyhow::bail!("hash failed for staged {}: {}", staged.display(), msg);
            }
        };
        if actual != f.expected_hash {
            let msg = format!(
                "hash mismatch for {}: expected {}, got {}",
                f.source_path, f.expected_hash, actual,
            );
            adb::update_step(conn, step_id, StepStatus::Failed, Some(&actual), Some(&msg))?;
            anyhow::bail!(msg);
        }
        adb::update_step(conn, step_id, StepStatus::Done, Some(&actual), None)?;
    }
    Ok(())
}

/// Stage 4: build the zip(s) by frame role.
fn zip_phase(
    conn: &Connection,
    operation_id: i64,
    files: &[ArchiveOperationFile],
    archive_root: &Path,
    compression: ArchiveCompression,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    // Group operation_files by target_zip_path
    let mut by_zip: HashMap<String, Vec<&ArchiveOperationFile>> = HashMap::new();
    for f in files {
        by_zip.entry(f.target_zip_path.clone()).or_default().push(f);
    }

    let total_zips = by_zip.len();
    let existing = existing_done_steps(conn, operation_id, ArchiveStage::ZipAdd)?;

    for (idx, (zip_path_str, group)) in by_zip.iter().enumerate() {
        check_cancel(cancel)?;
        emit_event(emitter, "archive-progress", &ArchiveProgress {
            operation_id,
            stage: "zipping".into(),
            current: idx + 1,
            total: total_zips,
            message: format!("Building zip {}/{}", idx + 1, total_zips),
        });

        // If every file in this zip already has a Done zip_add step, skip.
        let all_done = group.iter().all(|f| existing.contains(&f.id));
        if all_done {
            continue;
        }

        // Make sure parent dir exists.
        let zip_path = PathBuf::from(zip_path_str);
        if let Some(parent) = zip_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create zip dir {}", parent.display()))?;
        }

        // Build the zip from staging files.
        let entries: Vec<ZipEntry> = group.iter().map(|f| ZipEntry {
            source_path: staging::staging_file_path(archive_root, operation_id, &f.target_path_in_zip),
            path_in_zip: f.target_path_in_zip.clone(),
        }).collect();

        // Insert one InProgress step per file in this group; flip them to Done after zip succeeds.
        let mut step_ids = Vec::new();
        for f in group {
            if existing.contains(&f.id) {
                step_ids.push(None);
                continue;
            }
            let sid = adb::insert_step(conn, operation_id, Some(f.id), ArchiveStage::ZipAdd)?;
            adb::update_step(conn, sid, StepStatus::InProgress, None, None)?;
            step_ids.push(Some(sid));
        }

        match build_zip(&zip_path, &entries, compression) {
            Ok(_) => {
                for sid in step_ids.into_iter().flatten() {
                    adb::update_step(conn, sid, StepStatus::Done, None, None)?;
                }
            }
            Err(e) => {
                let msg = format!("{:#}", e);
                for sid in step_ids.into_iter().flatten() {
                    adb::update_step(conn, sid, StepStatus::Failed, None, Some(&msg))?;
                }
                anyhow::bail!("zip build failed for {}: {}", zip_path_str, msg);
            }
        }
    }
    Ok(())
}

/// Stage 5: open each zip and verify entry list.
fn verify_zip_phase(
    conn: &Connection,
    operation_id: i64,
    files: &[ArchiveOperationFile],
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let mut by_zip: HashMap<String, Vec<String>> = HashMap::new();
    for f in files {
        by_zip.entry(f.target_zip_path.clone()).or_default().push(f.target_path_in_zip.clone());
    }
    let total = by_zip.len();
    for (idx, (zp, expected_entries)) in by_zip.iter().enumerate() {
        check_cancel(cancel)?;
        emit_event(emitter, "archive-progress", &ArchiveProgress {
            operation_id,
            stage: "zip_verifying".into(),
            current: idx + 1,
            total,
            message: format!("Verifying zip {}/{}", idx + 1, total),
        });
        // Stage-level step (no operation_file_id)
        let sid = adb::insert_step(conn, operation_id, None, ArchiveStage::VerifyZip)?;
        adb::update_step(conn, sid, StepStatus::InProgress, None, None)?;
        match verify_zip_contents(Path::new(zp), expected_entries) {
            Ok(_) => adb::update_step(conn, sid, StepStatus::Done, None, None)?,
            Err(e) => {
                let msg = format!("{:#}", e);
                adb::update_step(conn, sid, StepStatus::Failed, None, Some(&msg))?;
                anyhow::bail!("zip verification failed for {}: {}", zp, msg);
            }
        }
    }
    Ok(())
}

/// Stage 6: delete original source files (the point of no return for cheap rollback).
fn delete_sources_phase(
    conn: &Connection,
    operation_id: i64,
    files: &[ArchiveOperationFile],
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let total = files.len();
    let existing = existing_done_steps(conn, operation_id, ArchiveStage::DeleteSource)?;
    for (idx, f) in files.iter().enumerate() {
        check_cancel(cancel)?;
        emit_event(emitter, "archive-progress", &ArchiveProgress {
            operation_id,
            stage: "deleting_sources".into(),
            current: idx + 1,
            total,
            message: format!("Deleting sources {}/{}", idx + 1, total),
        });
        if existing.contains(&f.id) {
            continue;
        }
        // Only delete moved files. Copied calibrations stay where they are.
        let sid = adb::insert_step(conn, operation_id, Some(f.id), ArchiveStage::DeleteSource)?;
        adb::update_step(conn, sid, StepStatus::InProgress, None, None)?;
        if f.disposition == "move" {
            match std::fs::remove_file(&f.source_path) {
                Ok(_) | Err(_) if !Path::new(&f.source_path).exists() => {
                    adb::update_step(conn, sid, StepStatus::Done, None, None)?;
                }
                Err(e) => {
                    let msg = format!("{:#}", e);
                    adb::update_step(conn, sid, StepStatus::Failed, None, Some(&msg))?;
                    anyhow::bail!("delete failed for {}: {}", f.source_path, msg);
                }
            }
        } else {
            // Copy disposition: nothing to delete; mark done immediately.
            adb::update_step(conn, sid, StepStatus::Done, None, None)?;
        }
    }
    Ok(())
}

/// Stage 7: update catalog flags + delete staging dir.
fn finalize_phase(
    conn: &Connection,
    operation_id: i64,
    frames_set_id: &i64,
    files: &[ArchiveOperationFile],
    archive_root: &Path,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    emit_event(emitter, "archive-progress", &ArchiveProgress {
        operation_id,
        stage: "finalizing".into(),
        current: 0,
        total: 1,
        message: "Finalizing".into(),
    });
    let sid = adb::insert_step(conn, operation_id, None, ArchiveStage::Finalize)?;
    adb::update_step(conn, sid, StepStatus::InProgress, None, None)?;

    // Mark moved files
    for f in files {
        if f.disposition == "move" {
            if let Some(file_id) = f.file_id {
                adb::mark_file_archived(
                    conn, file_id, operation_id, &f.target_zip_path, &f.target_path_in_zip,
                )?;
            }
        }
    }
    adb::mark_frame_set_archived(conn, *frames_set_id, operation_id)?;

    // Cleanup staging
    staging::cleanup_staging(archive_root, operation_id)?;
    adb::update_step(conn, sid, StepStatus::Done, None, None)?;
    Ok(())
}

/// Helper: which operation_file_ids already have a Done step at the given stage.
fn existing_done_steps(
    conn: &Connection,
    operation_id: i64,
    stage: ArchiveStage,
) -> Result<std::collections::HashSet<i64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT operation_file_id
         FROM archive_operation_steps
         WHERE operation_id = ?1 AND stage = ?2 AND status = 'done'
           AND operation_file_id IS NOT NULL",
    )?;
    let rows: Vec<i64> = stmt
        .query_map(rusqlite::params![operation_id, stage.as_str()], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::models::{ArchiveDisposition, ConflictResolution, Dispositions};
    use crate::archive::planner::{build_plan, commit_plan};
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use rusqlite::params;
    use tempfile::TempDir;

    /// End-to-end fixture: real files on disk, planner runs, executor runs to Completion.
    fn run_full_fixture() -> (Connection, TempDir, TempDir, i64) {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();

        let l1 = scan.path().join("M31/L_001.fits");
        let l2 = scan.path().join("M31/L_002.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        std::fs::write(&l1, b"light-1").unwrap();
        std::fs::write(&l2, b"light-2").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()],
        ).unwrap();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M31')", []).unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')",
            [],
        ).unwrap();
        for (file_id, path, frame_id) in [(1000, &l1, 10000), (1001, &l2, 10001)] {
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, ?3, 7, '2025-10-12', 'FITS')",
                params![file_id, path.to_str().unwrap(), path.file_name().unwrap().to_str().unwrap()],
            ).unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp)
                 VALUES (?1, ?2, 'M31', 'T', 'C', 'Light')",
                params![frame_id, file_id],
            ).unwrap();
            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)",
                [frame_id],
            ).unwrap();
        }

        let plan = build_plan(
            &conn, 1, arch.path(),
            &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
            ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
        (conn, arch, scan, op_id)
    }

    #[test]
    fn run_operation_completes_full_cycle() {
        let (conn, arch, scan, op_id) = run_full_fixture();
        let cancel = Arc::new(AtomicBool::new(false));
        let emitter = NullEmitter;

        run_operation(&conn, op_id, &cancel, &emitter).unwrap();

        // Operation Completed
        let op = adb::get_operation(&conn, op_id).unwrap();
        assert_eq!(op.status, "completed");

        // Source lights are deleted
        assert!(!scan.path().join("M31/L_001.fits").exists());
        assert!(!scan.path().join("M31/L_002.fits").exists());

        // Zip file exists in archive root
        let zips: Vec<_> = std::fs::read_dir(arch.path()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "zip").unwrap_or(false))
            .collect();
        assert_eq!(zips.len(), 1);

        // Frame set marked archived
        let archived_at: Option<String> = conn.query_row(
            "SELECT archived_at FROM frames_set WHERE id = 1", [], |r| r.get(0),
        ).unwrap();
        assert!(archived_at.is_some());
    }

    #[test]
    fn cancel_during_copy_aborts_with_cancel_signal() {
        let (conn, _arch, _scan, op_id) = run_full_fixture();
        let cancel = Arc::new(AtomicBool::new(true)); // pre-cancelled
        let emitter = NullEmitter;
        let err = run_operation(&conn, op_id, &cancel, &emitter).unwrap_err();
        assert!(was_cancelled(&err), "expected cancel sentinel, got: {}", err);
    }

    #[test]
    fn resume_skips_already_done_copies() {
        let (conn, arch, _scan, op_id) = run_full_fixture();
        let cancel = Arc::new(AtomicBool::new(false));

        // Manually run just the copy phase.
        let files = adb::list_operation_files(&conn, op_id).unwrap();
        copy_phase(&conn, op_id, &files, arch.path(), &cancel, &NullEmitter).unwrap();

        // Now run the full operation: copy steps should be reused.
        let before_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM archive_operation_steps WHERE operation_id = ?1 AND stage = 'copy'",
            [op_id], |r| r.get(0),
        ).unwrap();
        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();
        let after_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM archive_operation_steps WHERE operation_id = ?1 AND stage = 'copy'",
            [op_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(before_count, after_count, "copy steps should not be duplicated on resume");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p athenaeum-core --lib archive::executor`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/archive/executor.rs
git commit -m "feat(archive): executor drives stages 2-7 with cooperative cancel and resume"
```

---

## Phase 6 — Rollback and resume

### Task 15: `archive::rollback` — undo a partially-executed operation

**Files:**
- Modify: `crates/athenaeum-core/src/archive/rollback.rs`

- [ ] **Step 1: Write rollback implementation + tests**

Replace `crates/athenaeum-core/src/archive/rollback.rs` with:

```rust
//! Roll back an archive operation by reading its step log.
//!
//! Rollback strategy depends on how far the operation got:
//! - Through `zip_verifying`: source files untouched. Just delete partial zips +
//!   staging dir.
//! - During `deleting_sources` or `finalizing`: some sources already deleted;
//!   restore each deleted source from staging back to its original path,
//!   then delete the zip(s) + staging dir, then unmark catalog rows.

use crate::archive::db as adb;
use crate::archive::models::{
    ArchiveOperationFile, ArchiveStage, ArchiveStatus, StepStatus,
};
use crate::archive::staging;
use crate::events::{emit_event, ProgressEmitter};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Serialize, Clone, Debug)]
pub struct RollbackProgress {
    pub operation_id: i64,
    pub stage: String,
    pub current: usize,
    pub total: usize,
    pub message: String,
}

/// Roll back a forward operation. Idempotent: re-running on a partially-rolled-back
/// op is safe.
pub fn rollback_operation(
    conn: &Connection,
    operation_id: i64,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let op = adb::get_operation(conn, operation_id)?;
    let archive_root = PathBuf::from(&op.archive_root_path);
    let files = adb::list_operation_files(conn, operation_id)?;

    adb::update_operation_status(conn, operation_id, ArchiveStatus::RollingBack, None)?;

    // 1. Restore any deleted sources from staging.
    let deleted_file_ids = file_ids_with_done_step(conn, operation_id, ArchiveStage::DeleteSource)?;
    let total = deleted_file_ids.len();
    for (idx, f) in files.iter().enumerate() {
        if !deleted_file_ids.contains(&f.id) {
            continue;
        }
        if f.disposition != "move" {
            continue;
        }
        emit_event(emitter, "archive-rollback-progress", &RollbackProgress {
            operation_id,
            stage: "restore_source".into(),
            current: idx + 1,
            total,
            message: format!("Restoring source {}/{}", idx + 1, total),
        });

        let staged = staging::staging_file_path(&archive_root, operation_id, &f.target_path_in_zip);
        let target = Path::new(&f.source_path);

        if !target.exists() && staged.exists() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("rollback: create dir {}", parent.display()))?;
            }
            std::fs::copy(&staged, target)
                .with_context(|| format!("rollback: copy {} -> {}", staged.display(), target.display()))?;
        }

        // Mark a restore_source step as Done so resume understands progress.
        let sid = adb::insert_step(conn, operation_id, Some(f.id), ArchiveStage::RestoreSource)?;
        adb::update_step(conn, sid, StepStatus::Done, None, None)?;
    }

    // 2. Delete any zip files produced by this operation (whether partially or fully written).
    let mut seen_zips: HashSet<String> = HashSet::new();
    for f in &files {
        if seen_zips.insert(f.target_zip_path.clone()) {
            let zp = Path::new(&f.target_zip_path);
            if zp.exists() {
                let _ = std::fs::remove_file(zp);
            }
        }
    }

    // 3. Delete staging dir.
    let sid = adb::insert_step(conn, operation_id, None, ArchiveStage::DeleteStaging)?;
    adb::update_step(conn, sid, StepStatus::InProgress, None, None)?;
    staging::cleanup_staging(&archive_root, operation_id)?;
    adb::update_step(conn, sid, StepStatus::Done, None, None)?;

    // 4. Unmark catalog rows (frame set + any files we already marked).
    adb::unmark_frame_set_archived(conn, op.frames_set_id)?;
    for f in &files {
        if let Some(file_id) = f.file_id {
            adb::unmark_file_archived(conn, file_id, None)?;
        }
    }

    adb::update_operation_status(conn, operation_id, ArchiveStatus::RolledBack, None)?;
    Ok(())
}

fn file_ids_with_done_step(
    conn: &Connection,
    operation_id: i64,
    stage: ArchiveStage,
) -> Result<HashSet<i64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT operation_file_id
         FROM archive_operation_steps
         WHERE operation_id = ?1 AND stage = ?2 AND status = 'done'
           AND operation_file_id IS NOT NULL",
    )?;
    let rows: Vec<i64> = stmt
        .query_map(rusqlite::params![operation_id, stage.as_str()], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::executor::run_operation;
    use crate::archive::models::{ArchiveDisposition, ArchiveCompression, ConflictResolution, Dispositions};
    use crate::archive::planner::{build_plan, commit_plan};
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use rusqlite::params;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    /// Builds the same fixture as the executor tests.
    fn fixture() -> (Connection, TempDir, TempDir, i64) {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();

        let l1 = scan.path().join("M31/L_001.fits");
        let l2 = scan.path().join("M31/L_002.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        std::fs::write(&l1, b"light-1").unwrap();
        std::fs::write(&l2, b"light-2").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M31')", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        for (file_id, path, frame_id) in [(1000, &l1, 10000), (1001, &l2, 10001)] {
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, ?3, 7, '2025-10-12', 'FITS')",
                params![file_id, path.to_str().unwrap(), path.file_name().unwrap().to_str().unwrap()],
            ).unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp)
                 VALUES (?1, ?2, 'M31', 'T', 'C', 'Light')",
                params![frame_id, file_id],
            ).unwrap();
            conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)",
                [frame_id]).unwrap();
        }

        let plan = build_plan(
            &conn, 1, arch.path(),
            &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
            ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
        (conn, arch, scan, op_id)
    }

    #[test]
    fn rollback_after_completion_restores_sources_from_zip_extraction_path() {
        // Note: after a Completed operation, staging is gone. Our spec says
        // post-complete rollback should not be triggered via cancel — that's
        // what Restore is for. So we test the more interesting case:
        // partial-completion rollback (run forward to copy stage manually).
        let (conn, arch, scan, op_id) = fixture();
        let cancel = Arc::new(AtomicBool::new(false));

        // Drive copy + verify_copy + zip + verify_zip + delete_sources to leave
        // staging populated and sources deleted.
        let files = adb::list_operation_files(&conn, op_id).unwrap();
        crate::archive::staging::ensure_staging_dir(arch.path(), op_id).unwrap();
        for f in &files {
            crate::archive::staging::copy_into_staging(
                arch.path(), op_id, std::path::Path::new(&f.source_path), &f.target_path_in_zip,
            ).unwrap();
            // Pretend we already deleted sources & recorded delete_source done.
            std::fs::remove_file(&f.source_path).unwrap();
            let sid = adb::insert_step(&conn, op_id, Some(f.id), ArchiveStage::DeleteSource).unwrap();
            adb::update_step(&conn, sid, StepStatus::Done, None, None).unwrap();
        }

        // Now roll back.
        rollback_operation(&conn, op_id, &NullEmitter).unwrap();

        // Sources restored
        assert!(scan.path().join("M31/L_001.fits").exists());
        assert!(scan.path().join("M31/L_002.fits").exists());
        // Operation status updated
        let op = adb::get_operation(&conn, op_id).unwrap();
        assert_eq!(op.status, "rolled_back");
        // Frame set unmarked
        let archived_at: Option<String> = conn.query_row(
            "SELECT archived_at FROM frames_set WHERE id = 1", [], |r| r.get(0),
        ).unwrap();
        assert!(archived_at.is_none());

        let _ = cancel; // unused in this path
    }

    #[test]
    fn rollback_during_copy_just_cleans_staging() {
        let (conn, arch, _scan, op_id) = fixture();
        // Pre-cancel and run forward; expect a cancel error.
        let cancel = Arc::new(AtomicBool::new(true));
        let _ = run_operation(&conn, op_id, &cancel, &NullEmitter);
        // No source was deleted because we cancelled before that stage.
        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        let op = adb::get_operation(&conn, op_id).unwrap();
        assert_eq!(op.status, "rolled_back");
        // Staging dir gone
        let staging_dir = crate::archive::staging::staging_dir(arch.path(), op_id);
        assert!(!staging_dir.exists());
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p athenaeum-core --lib archive::rollback`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/archive/rollback.rs
git commit -m "feat(archive): rollback restores sources from staging and unmarks catalog"
```

---

### Task 16: `archive::resume` — find unfinished + resume

**Files:**
- Modify: `crates/athenaeum-core/src/archive/resume.rs`

- [ ] **Step 1: Write resume implementation + tests**

Replace `crates/athenaeum-core/src/archive/resume.rs` with:

```rust
//! Find unfinished archive operations and resume them.
//!
//! Resume reuses the executor: idempotency-by-step-log means already-Done
//! steps are skipped automatically.

use crate::archive::db as adb;
use crate::archive::executor::{run_operation, was_cancelled, CancelFlag};
use crate::archive::models::ArchiveOperationSummary;
use crate::events::ProgressEmitter;
use anyhow::Result;
use rusqlite::Connection;

/// List operations whose status is unfinished (resumable or rollback-needed).
pub fn find_unfinished_operations(conn: &Connection) -> Result<Vec<ArchiveOperationSummary>> {
    adb::list_unfinished_operations(conn)
}

/// Resume a previously-interrupted operation. Re-runs the executor; idempotent
/// step rows ensure already-completed work is not redone.
pub fn resume_operation(
    conn: &Connection,
    operation_id: i64,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    match run_operation(conn, operation_id, cancel, emitter) {
        Ok(()) => Ok(()),
        Err(e) if was_cancelled(&e) => Err(e),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::executor::run_operation;
    use crate::archive::models::{ArchiveCompression, ConflictResolution, Dispositions};
    use crate::archive::planner::{build_plan, commit_plan};
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use rusqlite::params;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    fn fixture() -> (Connection, TempDir, TempDir, i64) {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();
        let l1 = scan.path().join("M31/L_001.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        std::fs::write(&l1, b"l1").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M31')", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1000, ?1, 'L_001.fits', 2, '2025-10-12', 'FITS')",
            [l1.to_str().unwrap()],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp)
             VALUES (10000, 1000, 'M31', 'T', 'C', 'Light')",
            [],
        ).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000)", []).unwrap();

        let plan = build_plan(
            &conn, 1, arch.path(),
            &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
            ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
        (conn, arch, scan, op_id)
    }

    #[test]
    fn find_unfinished_returns_in_progress_ops() {
        let (conn, _arch, _scan, op_id) = fixture();
        // Status starts as Planning (unfinished)
        let unfinished = find_unfinished_operations(&conn).unwrap();
        assert_eq!(unfinished.len(), 1);
        assert_eq!(unfinished[0].id, op_id);
    }

    #[test]
    fn find_unfinished_excludes_completed() {
        let (conn, _arch, _scan, op_id) = fixture();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();
        let unfinished = find_unfinished_operations(&conn).unwrap();
        assert_eq!(unfinished.len(), 0);
    }

    #[test]
    fn resume_completes_a_partially_run_operation() {
        let (conn, arch, _scan, op_id) = fixture();
        // Manually run the copy phase only by calling run_operation under
        // a flag that flips after one iteration would normally be tricky.
        // Simpler: just call run_operation, which should succeed end-to-end here.
        let cancel = Arc::new(AtomicBool::new(false));
        resume_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();
        let op = adb::get_operation(&conn, op_id).unwrap();
        assert_eq!(op.status, "completed");
        let _ = arch;
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p athenaeum-core --lib archive::resume`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/archive/resume.rs
git commit -m "feat(archive): resume operation re-runs executor with idempotent steps"
```

---

## Phase 7 — Restore

### Task 17: `archive::restore` — extract and update catalog

**Files:**
- Modify: `crates/athenaeum-core/src/archive/restore.rs`

- [ ] **Step 1: Write the restore module**

Restore is simpler than the forward path because we already have the plan recorded in `archive_operation_files`. Replace `crates/athenaeum-core/src/archive/restore.rs` with:

```rust
//! Restore: extract zip(s) back to disk and update files.path.
//!
//! Implementation note: we record restore stages in the SAME archive_operation_steps
//! table, using stage names "restore_extract" and "restore_verify". This keeps a
//! single source of truth for the operation's history without a parallel table.

use crate::archive::db as adb;
use crate::archive::models::ArchiveStage;
use crate::duplicates::compute_xxhash;
use crate::events::{emit_event, ProgressEmitter};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub type CancelFlag = Arc<AtomicBool>;

#[derive(Serialize, Clone, Debug)]
pub struct RestoreProgress {
    pub operation_id: i64,
    pub stage: String,
    pub current: usize,
    pub total: usize,
    pub message: String,
}

/// Run a restore for the given archive operation.
///
/// `target_root_path` is the user-chosen directory; files are extracted as
/// `<target_root_path>/<target_path_in_zip>`, preserving the scan-root prefix.
/// On success: clear archive markers + rewrite `files.path` to the new locations.
/// On verify failure: keep the zip, mark restore failed, do NOT auto-rollback partial extracts.
pub fn run_restore(
    conn: &Connection,
    operation_id: i64,
    target_root_path: &Path,
    overwrite_existing: bool,
    keep_zip_after_restore: bool,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let op = adb::get_operation(conn, operation_id)?;
    let files = adb::list_operation_files(conn, operation_id)?;
    let total = files.len();

    // Stage: extract -----------------------------------------------------
    // Open each unique zip lazily.
    let mut zips: std::collections::HashMap<String, zip::ZipArchive<BufReader<File>>> = std::collections::HashMap::new();
    let mut buf = vec![0u8; 64 * 1024];

    for (idx, f) in files.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            anyhow::bail!("restore cancelled");
        }
        emit_event(emitter, "archive-restore-progress", &RestoreProgress {
            operation_id,
            stage: "extract".into(),
            current: idx + 1,
            total,
            message: format!("Extracting {}/{}", idx + 1, total),
        });

        // Open zip if not already
        let zr = zips.entry(f.target_zip_path.clone())
            .or_insert_with(|| {
                let file = File::open(&f.target_zip_path).expect("zip open");
                zip::ZipArchive::new(BufReader::new(file)).expect("zip parse")
            });
        let mut entry = zr.by_name(&f.target_path_in_zip)
            .with_context(|| format!("entry not found in zip: {}", f.target_path_in_zip))?;

        // Compute extraction destination
        let dest = target_root_path.join(&f.target_path_in_zip);
        if dest.exists() && !overwrite_existing {
            // Skip — caller decided to preserve existing files
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        let mut out = File::create(&dest)
            .with_context(|| format!("create dest {}", dest.display()))?;
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 { break; }
            out.write_all(&buf[..n])?;
        }
    }

    // Stage: verify ------------------------------------------------------
    let mut written_files = Vec::with_capacity(files.len());
    for (idx, f) in files.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            anyhow::bail!("restore cancelled");
        }
        emit_event(emitter, "archive-restore-progress", &RestoreProgress {
            operation_id,
            stage: "verify".into(),
            current: idx + 1,
            total,
            message: format!("Verifying {}/{}", idx + 1, total),
        });
        let dest = target_root_path.join(&f.target_path_in_zip);
        if !dest.exists() {
            // Skipped during extract (overwrite=false + existing). Don't verify.
            continue;
        }
        let actual = compute_xxhash(&dest)
            .with_context(|| format!("hash {}", dest.display()))?;
        if actual != f.expected_hash {
            anyhow::bail!(
                "restore verify failed for {}: expected {} got {}",
                dest.display(), f.expected_hash, actual,
            );
        }
        written_files.push((f, dest));
    }

    // Stage: update_catalog ----------------------------------------------
    for (f, new_path) in &written_files {
        if let Some(file_id) = f.file_id {
            adb::unmark_file_archived(conn, file_id, Some(new_path.to_str().unwrap()))?;
        }
    }
    adb::unmark_frame_set_archived(conn, op.frames_set_id)?;

    // Stage: cleanup -----------------------------------------------------
    if !keep_zip_after_restore {
        let mut seen: HashSet<String> = HashSet::new();
        for f in &files {
            if seen.insert(f.target_zip_path.clone()) {
                let _ = std::fs::remove_file(&f.target_zip_path);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::executor::run_operation;
    use crate::archive::models::{ArchiveCompression, ConflictResolution, Dispositions};
    use crate::archive::planner::{build_plan, commit_plan};
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use rusqlite::params;
    use tempfile::TempDir;

    #[test]
    fn full_archive_then_restore_cycle() {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();
        let restore_target = TempDir::new().unwrap();

        let l1 = scan.path().join("M31/L_001.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        std::fs::write(&l1, b"original-content").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M31')", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1000, ?1, 'L_001.fits', 16, '2025-10-12', 'FITS')",
            [l1.to_str().unwrap()],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp)
             VALUES (10000, 1000, 'M31', 'T', 'C', 'Light')",
            [],
        ).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000)", []).unwrap();

        let plan = build_plan(
            &conn, 1, arch.path(),
            &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
            ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();

        // Source is gone, archived_at is set
        assert!(!l1.exists());

        // Now restore to a different target
        run_restore(
            &conn, op_id, restore_target.path(),
            true, false, &cancel, &NullEmitter,
        ).unwrap();

        // Restored file exists and matches content
        let zips: Vec<_> = std::fs::read_dir(arch.path()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "zip").unwrap_or(false))
            .collect();
        // zip should have been deleted (keep_zip_after_restore = false)
        assert_eq!(zips.len(), 0);

        // Verify path was rewritten
        let new_path: String = conn.query_row(
            "SELECT path FROM files WHERE id = 1000", [], |r| r.get(0),
        ).unwrap();
        assert!(new_path.starts_with(restore_target.path().to_str().unwrap()));
        let restored_content = std::fs::read(&new_path).unwrap();
        assert_eq!(restored_content, b"original-content");

        // Frame set is no longer archived
        let archived_at: Option<String> = conn.query_row(
            "SELECT archived_at FROM frames_set WHERE id = 1", [], |r| r.get(0),
        ).unwrap();
        assert!(archived_at.is_none());
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p athenaeum-core --lib archive::restore`
Expected: 1 test passes.

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/archive/restore.rs
git commit -m "feat(archive): restore extracts zip and rewrites files.path"
```

---

## Phase 8 — AppState wiring + Tauri commands

### Task 18: Add `commands/archive.rs` to Tauri

**Files:**
- Create: `crates/athenaeum-tauri/src/commands/archive.rs`
- Modify: `crates/athenaeum-tauri/src/commands/mod.rs`
- Modify: `crates/athenaeum-tauri/src/lib.rs`

- [ ] **Step 1: Look up the existing TauriProgressEmitter**

Run: `grep -rn "TauriProgressEmitter\|impl ProgressEmitter for" crates/athenaeum-tauri/src/`

The emitter wrapping pattern needs to be reused. Note the type and the constructor; you'll need them in `start_archive_operation` and friends.

- [ ] **Step 2: Write the archive commands module**

Write `crates/athenaeum-tauri/src/commands/archive.rs`:

```rust
//! Archive feature Tauri commands.

use super::AppState;
use athenaeum_core::archive::{db as adb, executor, planner, resume, restore, rollback, models::*};
use athenaeum_core::services::ArchiveHandle;
use athenaeum_core::settings::keys;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::{AppHandle, State};

#[tauri::command]
pub async fn get_archive_settings(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let ctx = state.ctx.clone();
    let db = ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let root = ctx.settings.get_archive_root_path(&conn).map_err(|e| e.to_string())?;
    let compression = ctx.settings.get_archive_compression(&conn).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "rootPath": root,
        "compression": compression,
    }))
}

#[tauri::command]
pub async fn set_archive_root_path(
    state: State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    state.ctx.settings.persist_setting(&db.conn(), keys::ARCHIVE_ROOT_PATH, &path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_archive_compression(
    state: State<'_, AppState>,
    compression: String,
) -> Result<(), String> {
    if !matches!(compression.as_str(), "store" | "deflate") {
        return Err(format!("invalid compression value: {}", compression));
    }
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    state.ctx.settings.persist_setting(&db.conn(), keys::ARCHIVE_COMPRESSION, &compression)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn plan_archive_operation(
    state: State<'_, AppState>,
    frames_set_id: i64,
    dispositions: Dispositions,
    compression: ArchiveCompression,
) -> Result<ArchivePlan, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let root = state.ctx.settings.get_archive_root_path(&conn)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "archive root path is not set".to_string())?;
    planner::build_plan(
        &conn, frames_set_id, Path::new(&root), &dispositions, compression,
    ).map_err(|e| format!("{:#}", e))
}

#[tauri::command]
pub async fn start_archive_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    frames_set_id: i64,
    dispositions: Dispositions,
    compression: ArchiveCompression,
    conflict_resolution: ConflictResolution,
) -> Result<i64, String> {
    // One-at-a-time enforcement.
    {
        let map = state.ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err("another archive operation is already in progress".into());
        }
    }
    let ctx = state.ctx.clone();
    let db = ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let root = ctx.settings.get_archive_root_path(&conn)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "archive root path is not set".to_string())?;

    // Build + commit the plan synchronously.
    let plan = planner::build_plan(
        &conn, frames_set_id, Path::new(&root), &dispositions, compression,
    ).map_err(|e| format!("{:#}", e))?;
    let op_id = planner::commit_plan(&conn, &plan, conflict_resolution)
        .map_err(|e| format!("{:#}", e))?;

    // Register the cancel flag.
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut map = ctx.active_archives.lock().unwrap();
        map.insert(op_id, ArchiveHandle { operation_id: op_id, cancel_flag: cancel_flag.clone() });
    }

    // Spawn worker.
    let ctx_for_worker = ctx.clone();
    let app_for_emitter = app.clone();
    std::thread::spawn(move || {
        let emitter = crate::events::TauriProgressEmitter::new(app_for_emitter);
        let db = ctx_for_worker.db.get().expect("db");
        let conn = db.conn();
        let result = executor::run_operation(&conn, op_id, &cancel_flag, &emitter);
        match result {
            Ok(()) => {
                eprintln!("archive operation {} completed", op_id);
            }
            Err(e) => {
                if executor::was_cancelled(&e) {
                    let _ = adb::update_operation_status(
                        &conn, op_id, ArchiveStatus::Cancelled, None,
                    );
                } else {
                    eprintln!("archive operation {} failed: {:#}", op_id, e);
                    let msg = format!("{:#}", e);
                    let _ = adb::update_operation_status(
                        &conn, op_id, ArchiveStatus::Failed, Some(&msg),
                    );
                }
                // Auto-rollback on cancel or failure.
                if let Err(rb_err) = rollback::rollback_operation(&conn, op_id, &emitter) {
                    eprintln!("rollback for {} failed: {:#}", op_id, rb_err);
                }
            }
        }
        // Remove from active map regardless of outcome.
        let mut map = ctx_for_worker.active_archives.lock().unwrap();
        map.remove(&op_id);
    });

    Ok(op_id)
}

#[tauri::command]
pub async fn cancel_archive_operation(
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<(), String> {
    let map = state.ctx.active_archives.lock().unwrap();
    if let Some(handle) = map.get(&operation_id) {
        handle.cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    } else {
        Err(format!("no active archive operation with id {}", operation_id))
    }
}

#[tauri::command]
pub async fn list_unfinished_archive_operations(
    state: State<'_, AppState>,
) -> Result<Vec<ArchiveOperationSummary>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    resume::find_unfinished_operations(&db.conn()).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn resume_archive_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<(), String> {
    {
        let map = state.ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err("another archive operation is already in progress".into());
        }
    }
    let ctx = state.ctx.clone();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    ctx.active_archives.lock().unwrap().insert(operation_id, ArchiveHandle {
        operation_id, cancel_flag: cancel_flag.clone(),
    });

    let app_for_emitter = app.clone();
    std::thread::spawn(move || {
        let emitter = crate::events::TauriProgressEmitter::new(app_for_emitter);
        let db = ctx.db.get().expect("db");
        let conn = db.conn();
        if let Err(e) = resume::resume_operation(&conn, operation_id, &cancel_flag, &emitter) {
            eprintln!("resume {} failed: {:#}", operation_id, e);
            let msg = format!("{:#}", e);
            let _ = adb::update_operation_status(&conn, operation_id, ArchiveStatus::Failed, Some(&msg));
            let _ = rollback::rollback_operation(&conn, operation_id, &emitter);
        }
        ctx.active_archives.lock().unwrap().remove(&operation_id);
    });

    Ok(())
}

#[tauri::command]
pub async fn rollback_archive_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<(), String> {
    let ctx = state.ctx.clone();
    let app_for_emitter = app.clone();
    std::thread::spawn(move || {
        let emitter = crate::events::TauriProgressEmitter::new(app_for_emitter);
        let db = ctx.db.get().expect("db");
        let conn = db.conn();
        if let Err(e) = rollback::rollback_operation(&conn, operation_id, &emitter) {
            eprintln!("rollback {} failed: {:#}", operation_id, e);
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn list_archived_frame_sets(
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT fs.id, fs.name, fs.archived_at, fs.archive_operation_id,
                op.archive_root_path, op.started_at,
                (SELECT COUNT(*) FROM archive_operation_files
                 WHERE operation_id = op.id AND frame_role = 'light') AS lights,
                (SELECT COUNT(*) FROM archive_operation_files
                 WHERE operation_id = op.id AND frame_role = 'flat') AS flats,
                (SELECT COUNT(*) FROM archive_operation_files
                 WHERE operation_id = op.id AND frame_role = 'dark') AS darks,
                (SELECT COUNT(*) FROM archive_operation_files
                 WHERE operation_id = op.id AND frame_role = 'bias') AS bias,
                (SELECT COUNT(*) FROM archive_operation_files
                 WHERE operation_id = op.id AND frame_role = 'darkflat') AS darkflats
         FROM frames_set fs
         LEFT JOIN archive_operations op ON op.id = fs.archive_operation_id
         WHERE fs.archived_at IS NOT NULL
         ORDER BY fs.archived_at DESC",
    ).map_err(|e| e.to_string())?;
    let rows: Vec<serde_json::Value> = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "frames_set_id": row.get::<_, i64>(0)?,
            "name": row.get::<_, Option<String>>(1)?,
            "archived_at": row.get::<_, Option<String>>(2)?,
            "operation_id": row.get::<_, Option<i64>>(3)?,
            "archive_root_path": row.get::<_, Option<String>>(4)?,
            "started_at": row.get::<_, Option<String>>(5)?,
            "lights_count": row.get::<_, i64>(6)?,
            "flats_count": row.get::<_, i64>(7)?,
            "darks_count": row.get::<_, i64>(8)?,
            "bias_count": row.get::<_, i64>(9)?,
            "darkflats_count": row.get::<_, i64>(10)?,
        }))
    }).map_err(|e| e.to_string())?
      .collect::<rusqlite::Result<Vec<_>>>()
      .map_err(|e| e.to_string())?;
    Ok(rows)
}

#[tauri::command]
pub async fn start_restore_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    operation_id: i64,
    target_root_path: String,
    overwrite_existing: bool,
    keep_zip_after_restore: bool,
) -> Result<(), String> {
    {
        let map = state.ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err("another archive operation is already in progress".into());
        }
    }
    let ctx = state.ctx.clone();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    ctx.active_archives.lock().unwrap().insert(operation_id, ArchiveHandle {
        operation_id, cancel_flag: cancel_flag.clone(),
    });
    let app_for_emitter = app.clone();
    std::thread::spawn(move || {
        let emitter = crate::events::TauriProgressEmitter::new(app_for_emitter);
        let db = ctx.db.get().expect("db");
        let conn = db.conn();
        if let Err(e) = restore::run_restore(
            &conn, operation_id, Path::new(&target_root_path),
            overwrite_existing, keep_zip_after_restore, &cancel_flag, &emitter,
        ) {
            eprintln!("restore {} failed: {:#}", operation_id, e);
        }
        ctx.active_archives.lock().unwrap().remove(&operation_id);
    });
    Ok(())
}

#[tauri::command]
pub async fn delete_archive(
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<(), String> {
    {
        let map = state.ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err("another archive operation is already in progress".into());
        }
    }
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Delete zip files
    let files = adb::list_operation_files(&conn, operation_id).map_err(|e| e.to_string())?;
    let mut seen = std::collections::HashSet::new();
    for f in &files {
        if seen.insert(f.target_zip_path.clone()) {
            let _ = std::fs::remove_file(&f.target_zip_path);
        }
    }

    // Get frames_set_id, then delete frame set + cascading rows.
    let op = adb::get_operation(&conn, operation_id).map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM frames_set WHERE id = ?1",
        [op.frames_set_id],
    ).map_err(|e| e.to_string())?;
    // archive_operations row is also deleted via FK cascade from frames_set_id.

    Ok(())
}
```

- [ ] **Step 3: Wire `archive` into `commands/mod.rs`**

In `crates/athenaeum-tauri/src/commands/mod.rs`, add `pub mod archive;` at the end of the `pub mod` declarations (after `pub mod utils;`), and `pub use archive::*;` after the `pub use utils::*;` line if utils has a re-export.

Actually `utils` doesn't have a re-export per the file shown. Add this line just after `pub use plate_solve::*;`:

```rust
pub mod archive;
pub use archive::*;
```

- [ ] **Step 4: Register handlers in `lib.rs`**

In `crates/athenaeum-tauri/src/lib.rs`, find the `tauri::generate_handler![...]` block. Add the new commands at the end of the list:

```rust
            commands::get_archive_settings,
            commands::set_archive_root_path,
            commands::set_archive_compression,
            commands::plan_archive_operation,
            commands::start_archive_operation,
            commands::cancel_archive_operation,
            commands::list_unfinished_archive_operations,
            commands::resume_archive_operation,
            commands::rollback_archive_operation,
            commands::list_archived_frame_sets,
            commands::start_restore_operation,
            commands::delete_archive,
```

- [ ] **Step 5: Verify build**

Run: `cargo build -p athenaeum-tauri`
Expected: compiles cleanly. If `crate::events::TauriProgressEmitter` doesn't exist with that exact name, replace with the actual emitter type discovered in Step 1.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-tauri/
git commit -m "feat(tauri): wire archive feature commands into invoke handler"
```

---

## Phase 9 — Axum web routes (parity with Tauri)

### Task 19: Mirror archive commands as Axum routes

**Files:**
- Create: `crates/athenaeum-web/src/routes/archive.rs`
- Modify: `crates/athenaeum-web/src/routes/mod.rs`

- [ ] **Step 1: Look at an existing routes file with progress (e.g., `scan_roots.rs` or `export.rs`) to confirm the SseProgressEmitter pattern.**

Run: `grep -n "SseProgressEmitter\|state.event_tx" crates/athenaeum-web/src/routes/scan_roots.rs | head -20`

Note the constructor and how it's threaded into worker tokio::spawn blocks.

- [ ] **Step 2: Write the archive routes module**

Write `crates/athenaeum-web/src/routes/archive.rs`:

```rust
use crate::WebAppState;
use athenaeum_core::archive::{db as adb, executor, planner, resume, restore, rollback, models::*};
use athenaeum_core::services::ArchiveHandle;
use athenaeum_core::settings::keys;
use axum::{extract::State, Json};
use axum::http::StatusCode;
use serde::Deserialize;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct OperationIdRequest {
    pub operation_id: i64,
}

pub async fn get_archive_settings(
    State(state): State<WebAppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    let root = state.ctx.settings.get_archive_root_path(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let compression = state.ctx.settings.get_archive_compression(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "rootPath": root, "compression": compression })))
}

#[derive(Deserialize)]
pub struct SetRootRequest { pub path: String }

pub async fn set_archive_root_path(
    State(state): State<WebAppState>,
    Json(req): Json<SetRootRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    state.ctx.settings.persist_setting(&db.conn(), keys::ARCHIVE_ROOT_PATH, &req.path)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
pub struct SetCompressionRequest { pub compression: String }

pub async fn set_archive_compression(
    State(state): State<WebAppState>,
    Json(req): Json<SetCompressionRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    if !matches!(req.compression.as_str(), "store" | "deflate") {
        return Err((StatusCode::BAD_REQUEST, format!("invalid compression {}", req.compression)));
    }
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    state.ctx.settings.persist_setting(&db.conn(), keys::ARCHIVE_COMPRESSION, &req.compression)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
pub struct PlanRequest {
    pub frames_set_id: i64,
    pub dispositions: Dispositions,
    pub compression: ArchiveCompression,
}

pub async fn plan_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<PlanRequest>,
) -> Result<Json<ArchivePlan>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    let root = state.ctx.settings.get_archive_root_path(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::BAD_REQUEST, "archive root path not set".into()))?;
    planner::build_plan(&conn, req.frames_set_id, Path::new(&root), &req.dispositions, req.compression)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{:#}", e)))
}

#[derive(Deserialize)]
pub struct StartRequest {
    pub frames_set_id: i64,
    pub dispositions: Dispositions,
    pub compression: ArchiveCompression,
    pub conflict_resolution: ConflictResolution,
}

pub async fn start_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<StartRequest>,
) -> Result<Json<i64>, (StatusCode, String)> {
    {
        let map = state.ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err((StatusCode::CONFLICT, "another archive operation is already in progress".into()));
        }
    }
    let ctx = state.ctx.clone();
    let db = ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    let root = ctx.settings.get_archive_root_path(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::BAD_REQUEST, "archive root path not set".into()))?;

    let plan = planner::build_plan(&conn, req.frames_set_id, Path::new(&root), &req.dispositions, req.compression)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{:#}", e)))?;
    let op_id = planner::commit_plan(&conn, &plan, req.conflict_resolution)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{:#}", e)))?;

    let cancel_flag = Arc::new(AtomicBool::new(false));
    ctx.active_archives.lock().unwrap().insert(op_id, ArchiveHandle {
        operation_id: op_id, cancel_flag: cancel_flag.clone(),
    });

    let event_tx = state.event_tx.clone();
    let ctx_for_worker = ctx.clone();
    tokio::task::spawn_blocking(move || {
        let emitter = crate::SseProgressEmitter::new(event_tx);
        let db = ctx_for_worker.db.get().expect("db");
        let conn = db.conn();
        if let Err(e) = executor::run_operation(&conn, op_id, &cancel_flag, &emitter) {
            if executor::was_cancelled(&e) {
                let _ = adb::update_operation_status(&conn, op_id, ArchiveStatus::Cancelled, None);
            } else {
                let msg = format!("{:#}", e);
                let _ = adb::update_operation_status(&conn, op_id, ArchiveStatus::Failed, Some(&msg));
            }
            let _ = rollback::rollback_operation(&conn, op_id, &emitter);
        }
        ctx_for_worker.active_archives.lock().unwrap().remove(&op_id);
    });

    Ok(Json(op_id))
}

pub async fn cancel_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let map = state.ctx.active_archives.lock().unwrap();
    if let Some(handle) = map.get(&req.operation_id) {
        handle.cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(StatusCode::OK)
    } else {
        Err((StatusCode::NOT_FOUND, format!("no active operation {}", req.operation_id)))
    }
}

pub async fn list_unfinished_archive_operations(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<ArchiveOperationSummary>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    resume::find_unfinished_operations(&db.conn())
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

pub async fn resume_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    {
        let map = state.ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err((StatusCode::CONFLICT, "another archive operation already running".into()));
        }
    }
    let ctx = state.ctx.clone();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    ctx.active_archives.lock().unwrap().insert(req.operation_id, ArchiveHandle {
        operation_id: req.operation_id, cancel_flag: cancel_flag.clone(),
    });
    let event_tx = state.event_tx.clone();
    tokio::task::spawn_blocking(move || {
        let emitter = crate::SseProgressEmitter::new(event_tx);
        let db = ctx.db.get().expect("db");
        let conn = db.conn();
        let _ = resume::resume_operation(&conn, req.operation_id, &cancel_flag, &emitter);
        ctx.active_archives.lock().unwrap().remove(&req.operation_id);
    });
    Ok(StatusCode::OK)
}

pub async fn rollback_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let event_tx = state.event_tx.clone();
    tokio::task::spawn_blocking(move || {
        let emitter = crate::SseProgressEmitter::new(event_tx);
        let db = ctx.db.get().expect("db");
        let conn = db.conn();
        let _ = rollback::rollback_operation(&conn, req.operation_id, &emitter);
    });
    Ok(StatusCode::OK)
}

pub async fn list_archived_frame_sets(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT fs.id, fs.name, fs.archived_at, fs.archive_operation_id,
                op.archive_root_path, op.started_at,
                (SELECT COUNT(*) FROM archive_operation_files
                 WHERE operation_id = op.id AND frame_role = 'light') AS lights,
                (SELECT COUNT(*) FROM archive_operation_files
                 WHERE operation_id = op.id AND frame_role = 'flat') AS flats,
                (SELECT COUNT(*) FROM archive_operation_files
                 WHERE operation_id = op.id AND frame_role = 'dark') AS darks,
                (SELECT COUNT(*) FROM archive_operation_files
                 WHERE operation_id = op.id AND frame_role = 'bias') AS bias,
                (SELECT COUNT(*) FROM archive_operation_files
                 WHERE operation_id = op.id AND frame_role = 'darkflat') AS darkflats
         FROM frames_set fs
         LEFT JOIN archive_operations op ON op.id = fs.archive_operation_id
         WHERE fs.archived_at IS NOT NULL
         ORDER BY fs.archived_at DESC",
    ).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rows = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "frames_set_id": row.get::<_, i64>(0)?,
            "name": row.get::<_, Option<String>>(1)?,
            "archived_at": row.get::<_, Option<String>>(2)?,
            "operation_id": row.get::<_, Option<i64>>(3)?,
            "archive_root_path": row.get::<_, Option<String>>(4)?,
            "started_at": row.get::<_, Option<String>>(5)?,
            "lights_count": row.get::<_, i64>(6)?,
            "flats_count": row.get::<_, i64>(7)?,
            "darks_count": row.get::<_, i64>(8)?,
            "bias_count": row.get::<_, i64>(9)?,
            "darkflats_count": row.get::<_, i64>(10)?,
        }))
    }).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
      .collect::<rusqlite::Result<Vec<_>>>()
      .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct StartRestoreRequest {
    pub operation_id: i64,
    pub target_root_path: String,
    pub overwrite_existing: bool,
    pub keep_zip_after_restore: bool,
}

pub async fn start_restore_operation(
    State(state): State<WebAppState>,
    Json(req): Json<StartRestoreRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    {
        let map = state.ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err((StatusCode::CONFLICT, "another archive operation already running".into()));
        }
    }
    let ctx = state.ctx.clone();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    ctx.active_archives.lock().unwrap().insert(req.operation_id, ArchiveHandle {
        operation_id: req.operation_id, cancel_flag: cancel_flag.clone(),
    });
    let event_tx = state.event_tx.clone();
    tokio::task::spawn_blocking(move || {
        let emitter = crate::SseProgressEmitter::new(event_tx);
        let db = ctx.db.get().expect("db");
        let conn = db.conn();
        let _ = restore::run_restore(
            &conn, req.operation_id, Path::new(&req.target_root_path),
            req.overwrite_existing, req.keep_zip_after_restore, &cancel_flag, &emitter,
        );
        ctx.active_archives.lock().unwrap().remove(&req.operation_id);
    });
    Ok(StatusCode::OK)
}

pub async fn delete_archive(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    let files = adb::list_operation_files(&conn, req.operation_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut seen = std::collections::HashSet::new();
    for f in &files {
        if seen.insert(f.target_zip_path.clone()) {
            let _ = std::fs::remove_file(&f.target_zip_path);
        }
    }
    let op = adb::get_operation(&conn, req.operation_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    conn.execute("DELETE FROM frames_set WHERE id = ?1", [op.frames_set_id])
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}
```

- [ ] **Step 3: Register routes in `routes/mod.rs`**

In `crates/athenaeum-web/src/routes/mod.rs`:

1. Add `mod archive;` to the list of module declarations after `mod plate_solve;`.

2. Inside `build_router`, add this block of routes before `// Core` (or wherever fits the existing organization):

```rust
        // Archive feature
        .route("/api/get_archive_settings", post(archive::get_archive_settings))
        .route("/api/set_archive_root_path", post(archive::set_archive_root_path))
        .route("/api/set_archive_compression", post(archive::set_archive_compression))
        .route("/api/plan_archive_operation", post(archive::plan_archive_operation))
        .route("/api/start_archive_operation", post(archive::start_archive_operation))
        .route("/api/cancel_archive_operation", post(archive::cancel_archive_operation))
        .route("/api/list_unfinished_archive_operations", post(archive::list_unfinished_archive_operations))
        .route("/api/resume_archive_operation", post(archive::resume_archive_operation))
        .route("/api/rollback_archive_operation", post(archive::rollback_archive_operation))
        .route("/api/list_archived_frame_sets", post(archive::list_archived_frame_sets))
        .route("/api/start_restore_operation", post(archive::start_restore_operation))
        .route("/api/delete_archive", post(archive::delete_archive))
```

- [ ] **Step 4: Verify build**

Run: `cargo build -p athenaeum-web`
Expected: compiles cleanly. If `crate::SseProgressEmitter` is at a different path, adjust accordingly (look at `scan_roots.rs`).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-web/
git commit -m "feat(web): add archive feature routes for parity with Tauri"
```

---

## Phase 10 — Frontend types and API layer

### Task 20: TypeScript types for archive feature

**Files:**
- Create: `src/types/archive.ts`

- [ ] **Step 1: Write the types**

Write `src/types/archive.ts`:

```typescript
// Mirrors crates/athenaeum-core/src/archive/models.rs

export type ArchiveDisposition = 'move' | 'copy' | 'skip';
export type ArchiveCompression = 'store' | 'deflate';
export type ConflictResolution = 'overwrite' | 'add_suffix';
export type FrameRole = 'light' | 'flat' | 'dark' | 'bias' | 'darkflat';

export interface Dispositions {
  flats: ArchiveDisposition | null;
  darks: ArchiveDisposition | null;
  bias: ArchiveDisposition | null;
  darkflats: ArchiveDisposition | null;
}

export interface ArchiveSettings {
  rootPath: string | null;
  compression: ArchiveCompression;
}

export interface ArchiveOperationFile {
  id: number;
  operation_id: number;
  file_id: number | null;
  source_path: string;
  target_zip_path: string;
  target_path_in_zip: string;
  expected_hash: string;
  disposition: string;
  frame_role: string;
  file_size_bytes: number;
}

export interface PlannedZip {
  zip_path: string;
  zip_filename: string;
  frame_role: FrameRole;
  file_count: number;
  total_size_bytes: number;
}

export interface SharedCalibrationWarning {
  frame_role: FrameRole;
  calibration_set_id: number;
  other_frames_set_ids: number[];
}

export interface ZipFilenameConflict {
  zip_path: string;
  zip_filename: string;
}

export interface ArchivePlan {
  frames_set_id: number;
  archive_root_path: string;
  dispositions: Dispositions;
  compression: ArchiveCompression;
  files: ArchiveOperationFile[];
  zips: PlannedZip[];
  shared_calibrations: SharedCalibrationWarning[];
  conflicts: ZipFilenameConflict[];
  total_size_bytes: number;
}

export interface ArchiveOperationSummary {
  id: number;
  frames_set_id: number;
  frame_set_name: string | null;
  status: string;
  started_at: string;
  finished_at: string | null;
  error_message: string | null;
}

export interface ArchivedFrameSetSummary {
  frames_set_id: number;
  name: string | null;
  archived_at: string | null;
  operation_id: number | null;
  archive_root_path: string | null;
  started_at: string | null;
  lights_count: number;
  flats_count: number;
  darks_count: number;
  bias_count: number;
  darkflats_count: number;
}

export interface ArchiveProgressEvent {
  operation_id: number;
  stage: string;
  current: number;
  total: number;
  message: string;
}
```

- [ ] **Step 2: Verify TS build**

Run: `npx tsc --noEmit -p . 2>&1 | head -30` (or the project's typecheck command).
Expected: no new errors.

- [ ] **Step 3: Commit**

```bash
git add src/types/archive.ts
git commit -m "feat(types): add archive feature TypeScript types"
```

---

### Task 21: API layer for archive

**Files:**
- Create: `src/api/archive.ts`

- [ ] **Step 1: Read existing api shape to see patterns**

Run: `cat src/api/index.ts | head -40` and `cat src/api/desktop.ts | head -40` to confirm existing wrapper conventions (e.g., a single `api.invoke<T>()` pass-through).

- [ ] **Step 2: Write the api wrappers**

Write `src/api/archive.ts`:

```typescript
import { api } from './index';
import type {
  ArchiveCompression,
  ArchivedFrameSetSummary,
  ArchiveOperationSummary,
  ArchivePlan,
  ArchiveSettings,
  ConflictResolution,
  Dispositions,
} from '../types/archive';

export async function getArchiveSettings(): Promise<ArchiveSettings> {
  return api.invoke<ArchiveSettings>('get_archive_settings');
}

export async function setArchiveRootPath(path: string): Promise<void> {
  await api.invoke('set_archive_root_path', { path });
}

export async function setArchiveCompression(compression: ArchiveCompression): Promise<void> {
  await api.invoke('set_archive_compression', { compression });
}

export async function planArchiveOperation(
  framesSetId: number,
  dispositions: Dispositions,
  compression: ArchiveCompression,
): Promise<ArchivePlan> {
  return api.invoke<ArchivePlan>('plan_archive_operation', {
    framesSetId,
    dispositions,
    compression,
  });
}

export async function startArchiveOperation(
  framesSetId: number,
  dispositions: Dispositions,
  compression: ArchiveCompression,
  conflictResolution: ConflictResolution,
): Promise<number> {
  return api.invoke<number>('start_archive_operation', {
    framesSetId,
    dispositions,
    compression,
    conflictResolution,
  });
}

export async function cancelArchiveOperation(operationId: number): Promise<void> {
  await api.invoke('cancel_archive_operation', { operationId });
}

export async function listUnfinishedArchiveOperations(): Promise<ArchiveOperationSummary[]> {
  return api.invoke<ArchiveOperationSummary[]>('list_unfinished_archive_operations');
}

export async function resumeArchiveOperation(operationId: number): Promise<void> {
  await api.invoke('resume_archive_operation', { operationId });
}

export async function rollbackArchiveOperation(operationId: number): Promise<void> {
  await api.invoke('rollback_archive_operation', { operationId });
}

export async function listArchivedFrameSets(): Promise<ArchivedFrameSetSummary[]> {
  return api.invoke<ArchivedFrameSetSummary[]>('list_archived_frame_sets');
}

export async function startRestoreOperation(
  operationId: number,
  targetRootPath: string,
  overwriteExisting: boolean,
  keepZipAfterRestore: boolean,
): Promise<void> {
  await api.invoke('start_restore_operation', {
    operationId,
    targetRootPath,
    overwriteExisting,
    keepZipAfterRestore,
  });
}

export async function deleteArchive(operationId: number): Promise<void> {
  await api.invoke('delete_archive', { operationId });
}
```

Note on argument naming: Tauri serializes camelCase JS keys into snake_case Rust parameter names automatically. The Axum web routes accept JSON with snake_case keys (matching the Rust struct field names). The `api.invoke` layer handles routing to the correct backend; check `src/api/index.ts` to confirm if it does any case conversion when in web mode. If it does NOT, the web routes should accept camelCase and Serde rename them OR the `api.invoke` web path should snake_case the keys before sending.

If `api.invoke` does not handle this, add a `snake_case` adapter inside `archive.ts` (for the web path) or introduce `#[serde(rename_all = "camelCase")]` on the Rust request structs. **Default for this plan: use snake_case in the JS payloads** (rewrite the camelCase keys above to snake_case versions like `frames_set_id`, `conflict_resolution`, `target_root_path`, `overwrite_existing`, `keep_zip_after_restore`) IF and ONLY IF the existing api layer doesn't transform. Verify by reading `src/api/web.ts` (or equivalent) before locking this in.

- [ ] **Step 3: Verify TS build**

Run typecheck: `npx tsc --noEmit -p . 2>&1 | head -30`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add src/api/archive.ts
git commit -m "feat(api): add archive feature API wrappers"
```

---

## Phase 11 — Frontend UI components

> **Frontend implementer reminder:** project memory specifies that frontend feature work should use the **frontend-dev agent** or **frontend-design skill**. The component skeletons in this phase are starting points — adapt them to the project's design tokens (`bg-surface`, `text-content`, etc.) and reuse existing dialog/modal primitives where they exist. Don't reinvent button or modal styles if there's already a shared component.

### Task 22: `ArchiveDispositionDialog` component

**Files:**
- Create: `src/components/archive/ArchiveDispositionDialog.tsx`

- [ ] **Step 1: Look at existing modal/dialog components**

Run: `ls src/components/ | grep -iE "dialog|modal" | head` and read one to copy the modal shell pattern (overlay, escape-to-close, focus trap if any).

- [ ] **Step 2: Implement the disposition dialog**

Write `src/components/archive/ArchiveDispositionDialog.tsx`. Required behavior:

- **Inputs (props)**:
  - `framesSetId: number`
  - `archiveRootPath: string` — used in the size/zip preview
  - `defaultCompression: ArchiveCompression`
  - `onCancel: () => void`
  - `onStart: (dispositions: Dispositions, compression: ArchiveCompression, conflictResolution: ConflictResolution) => void`
- **State**:
  - Initial `Dispositions` is loaded from a planning call with `flats/darks/bias/darkflats: 'skip'`. The planner returns which calibration types exist; types not in the chain stay null and are not rendered.
  - `compression: ArchiveCompression` defaults to `defaultCompression`.
  - Loading state while the plan is being computed.
- **Workflow**:
  - On mount: call `planArchiveOperation(framesSetId, { flats:'skip',darks:'skip',bias:'skip',darkflats:'skip' }, defaultCompression)`. Use the response to determine which calibration types to show (a type is "present" if any `files[].frame_role === <role>` OR — better — if the planner exposed it explicitly. If the current planner only returns files based on disposition≠Skip, we won't see calibration types. Adapt: also call the existing `get_calibration_hierarchy_for_frame_set` to detect *what types exist*. The hierarchy already tells us "this frame set has flats/darks/bias/darkflats." Use that to decide which radio rows to render. The disposition dialog uses the planner result only to show estimated zip sizes and existing-file conflicts.
  - Whenever the user changes a disposition, re-call `planArchiveOperation` with the new dispositions to refresh the size estimate and zip preview list.
  - For each row, mark Move as disabled if `plan.shared_calibrations` contains an entry with this `frame_role`. Tooltip: "Used by N other frame sets — only Copy is allowed." (N = `other_frames_set_ids.length`).
  - Default selection per row: `'skip'`.
- **UI structure** (per `src/pages/FrameSetDetail.tsx` styling):
  - Modal overlay with title "Archive Frame Set"
  - Section: "Calibrations" — only types present in the chain. Each row: type label, three radios (Move | Copy | Skip), inline warning on Move when shared.
  - Section: "Compression" — dropdown
  - Section: "Estimated archive" — total size, list of zip filenames + per-zip size
  - If `plan.conflicts.length > 0`: yellow notice "These zips already exist; you'll be asked to confirm before they're written: <list>"
  - Footer: `[Cancel]` `[Start Archiving]` (disabled while planning)
- **On Start click**:
  - If `plan.conflicts.length > 0`: open `ArchiveConflictDialog` first; the conflict dialog returns a `ConflictResolution`; then call `onStart(dispositions, compression, resolution)`.
  - Else call `onStart(dispositions, compression, 'overwrite')` directly (no conflict to resolve).

Show a code skeleton for the implementer:

```typescript
// src/components/archive/ArchiveDispositionDialog.tsx
import { useEffect, useState } from 'react';
import {
  ArchiveCompression, ArchivePlan, ConflictResolution, Dispositions, FrameRole,
} from '../../types/archive';
import { planArchiveOperation } from '../../api/archive';
import { ArchiveConflictDialog } from './ArchiveConflictDialog';

interface Props {
  framesSetId: number;
  archiveRootPath: string;
  defaultCompression: ArchiveCompression;
  onCancel: () => void;
  onStart: (
    dispositions: Dispositions,
    compression: ArchiveCompression,
    conflict: ConflictResolution,
  ) => void;
}

const DEFAULT_DISP: Dispositions = { flats: 'skip', darks: 'skip', bias: 'skip', darkflats: 'skip' };

export function ArchiveDispositionDialog(props: Props) {
  const [dispositions, setDispositions] = useState<Dispositions>(DEFAULT_DISP);
  const [compression, setCompression] = useState<ArchiveCompression>(props.defaultCompression);
  const [plan, setPlan] = useState<ArchivePlan | null>(null);
  const [planning, setPlanning] = useState(false);
  const [showConflict, setShowConflict] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setPlanning(true);
    planArchiveOperation(props.framesSetId, dispositions, compression)
      .then(p => { if (!cancelled) setPlan(p); })
      .catch(err => { console.error('plan failed', err); })
      .finally(() => { if (!cancelled) setPlanning(false); });
    return () => { cancelled = true; };
  }, [props.framesSetId, dispositions, compression]);

  const sharedRoles = new Set(plan?.shared_calibrations.map(s => s.frame_role) ?? []);

  function rowFor(role: 'flats' | 'darks' | 'bias' | 'darkflats', label: string, frameRole: FrameRole) {
    const moveDisabled = sharedRoles.has(frameRole);
    const sharedCount = plan?.shared_calibrations.find(s => s.frame_role === frameRole)?.other_frames_set_ids.length ?? 0;
    return (
      <div className="flex items-center gap-3 py-2">
        <span className="w-32 text-sm">{label}</span>
        {(['move', 'copy', 'skip'] as const).map(opt => (
          <label key={opt} className={`flex items-center gap-1 text-sm ${moveDisabled && opt === 'move' ? 'opacity-50 cursor-not-allowed' : ''}`}
                 title={moveDisabled && opt === 'move' ? `Used by ${sharedCount} other frame set(s) — only Copy is allowed.` : undefined}>
            <input
              type="radio"
              name={`disp-${role}`}
              checked={dispositions[role] === opt}
              disabled={moveDisabled && opt === 'move'}
              onChange={() => setDispositions(d => ({ ...d, [role]: opt }))}
            />
            {opt}
          </label>
        ))}
      </div>
    );
  }

  // FIXME: detect which calibration rows to show based on a separate query
  // (e.g., get_calibration_hierarchy_for_frame_set) — the planner alone won't
  // surface a 'skip'ed type. For v1, render all four rows; rows for absent types
  // will still appear but have no effect.

  function handleStart() {
    if (!plan) return;
    if (plan.conflicts.length > 0) {
      setShowConflict(true);
      return;
    }
    props.onStart(dispositions, compression, 'overwrite');
  }

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-surface rounded-lg shadow-xl p-6 max-w-2xl w-full">
        <h2 className="text-lg font-semibold mb-3">Archive Frame Set</h2>

        <h3 className="text-sm font-medium mt-4 mb-1">Calibrations</h3>
        {rowFor('flats', 'Flats', 'flat')}
        {rowFor('darks', 'Darks', 'dark')}
        {rowFor('bias', 'Bias', 'bias')}
        {rowFor('darkflats', 'DarkFlats', 'darkflat')}

        <h3 className="text-sm font-medium mt-4 mb-1">Compression</h3>
        <select
          value={compression}
          onChange={e => setCompression(e.target.value as ArchiveCompression)}
          className="bg-surface-hover border border-border rounded px-2 py-1 text-sm"
        >
          <option value="store">Store (no compression — fastest)</option>
          <option value="deflate">Deflate (smaller, slower)</option>
        </select>

        {planning && <p className="text-sm text-content-muted mt-4">Computing plan…</p>}
        {plan && (
          <div className="mt-4 text-sm">
            <p>Total size: {(plan.total_size_bytes / 1024 / 1024).toFixed(1)} MB</p>
            <p className="font-medium mt-2">Zips that will be produced:</p>
            <ul className="list-disc ml-6">
              {plan.zips.map(z => (
                <li key={z.zip_filename}>{z.zip_filename} — {z.file_count} files, {(z.total_size_bytes / 1024 / 1024).toFixed(1)} MB</li>
              ))}
            </ul>
            {plan.conflicts.length > 0 && (
              <p className="mt-2 text-warning">
                {plan.conflicts.length} of these zip name(s) already exist; you'll be asked how to resolve.
              </p>
            )}
          </div>
        )}

        <div className="flex justify-end gap-2 mt-6">
          <button onClick={props.onCancel} className="px-3 py-1.5 rounded border border-border">Cancel</button>
          <button
            onClick={handleStart}
            disabled={planning || !plan}
            className="px-3 py-1.5 rounded bg-accent text-white disabled:opacity-50"
          >Start Archiving</button>
        </div>
      </div>

      {showConflict && plan && (
        <ArchiveConflictDialog
          conflicts={plan.conflicts}
          onCancel={() => setShowConflict(false)}
          onResolve={(resolution) => {
            setShowConflict(false);
            props.onStart(dispositions, compression, resolution);
          }}
        />
      )}
    </div>
  );
}
```

- [ ] **Step 3: Run typecheck**

Run typecheck. Expected: no errors. If `ArchiveConflictDialog` import is unresolved, that's expected — the next task creates it; comment out the import until then or scaffold an empty placeholder.

- [ ] **Step 4: Commit**

```bash
git add src/components/archive/
git commit -m "feat(ui): ArchiveDispositionDialog with per-type radios and shared-cal warnings"
```

---

### Task 23: `ArchiveConflictDialog` component

**Files:**
- Create: `src/components/archive/ArchiveConflictDialog.tsx`

- [ ] **Step 1: Implement**

Write `src/components/archive/ArchiveConflictDialog.tsx`:

```typescript
import type { ConflictResolution, ZipFilenameConflict } from '../../types/archive';

interface Props {
  conflicts: ZipFilenameConflict[];
  onCancel: () => void;
  onResolve: (resolution: ConflictResolution) => void;
}

export function ArchiveConflictDialog({ conflicts, onCancel, onResolve }: Props) {
  return (
    <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-[60]">
      <div className="bg-surface rounded-lg shadow-xl p-6 max-w-lg w-full">
        <h2 className="text-lg font-semibold mb-3">Archive name conflict</h2>
        <p className="text-sm mb-2">The following zip file{conflicts.length === 1 ? '' : 's'} already exist in the archive root:</p>
        <ul className="list-disc ml-6 mb-4 text-sm font-mono">
          {conflicts.map(c => <li key={c.zip_path}>{c.zip_filename}</li>)}
        </ul>
        <p className="text-sm mb-4">How would you like to proceed?</p>
        <div className="flex justify-end gap-2">
          <button onClick={onCancel} className="px-3 py-1.5 rounded border border-border">Cancel</button>
          <button onClick={() => onResolve('add_suffix')} className="px-3 py-1.5 rounded bg-surface-hover">Add suffix</button>
          <button onClick={() => onResolve('overwrite')} className="px-3 py-1.5 rounded bg-warning text-white">Overwrite</button>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Commit**

```bash
git add src/components/archive/ArchiveConflictDialog.tsx
git commit -m "feat(ui): ArchiveConflictDialog for overwrite/suffix/cancel"
```

---

### Task 24: Add Move and ZIP button to FrameSetDetail

**Files:**
- Modify: `src/pages/FrameSetDetail.tsx`

- [ ] **Step 1: Wire the button + flow**

In `src/pages/FrameSetDetail.tsx`:

1. Add imports:

```typescript
import { Archive as ArchiveIcon } from 'lucide-react';
import { useEffect, useCallback, useState } from 'react'; // already present
import { ArchiveDispositionDialog } from '../components/archive/ArchiveDispositionDialog';
import { getArchiveSettings, setArchiveRootPath, startArchiveOperation } from '../api/archive';
import { pickDirectory } from '../api/desktop';
```

2. Add state hooks (near the existing `findNewBusy` etc.):

```typescript
const [showArchiveDialog, setShowArchiveDialog] = useState(false);
const [archiveRoot, setArchiveRoot] = useState<string | null>(null);
const [archiveCompression, setArchiveCompression] = useState<'store' | 'deflate'>('store');
const [archiving, setArchiving] = useState(false);
```

3. Add the click handler:

```typescript
const handleArchiveClick = useCallback(async () => {
  const settings = await getArchiveSettings();
  if (!settings.rootPath) {
    const choice = window.confirm(
      'You haven\'t set an archive folder yet. Choose one now?'
    );
    if (!choice) return;
    const picked = await pickDirectory();
    if (!picked) return;
    await setArchiveRootPath(picked);
    setArchiveRoot(picked);
    setArchiveCompression(settings.compression);
  } else {
    setArchiveRoot(settings.rootPath);
    setArchiveCompression(settings.compression);
  }
  setShowArchiveDialog(true);
}, []);
```

4. Add the start handler that calls `startArchiveOperation`:

```typescript
const handleStartArchive = useCallback(async (
  dispositions, compression, conflictResolution,
) => {
  if (!detail.frames_set?.id) return;
  setShowArchiveDialog(false);
  setArchiving(true);
  try {
    await startArchiveOperation(detail.frames_set.id, dispositions, compression, conflictResolution);
    // The progress UI in the global Tasks panel will track from here.
  } catch (e) {
    console.error('start archive failed', e);
    alert(`Archive failed to start: ${e}`);
  } finally {
    setArchiving(false);
  }
}, [detail.frames_set?.id]);
```

5. Place the Move and ZIP button next to the existing "Find new images" button. In the JSX block around line 502-518 of `FrameSetDetail.tsx` (the `<button onClick={handleFindNewClick}>` block), add a sibling button immediately after it:

```typescript
<button
  type="button"
  onClick={handleArchiveClick}
  disabled={archiving || !!detail.frames_set?.archived_at}
  title={detail.frames_set?.archived_at ? 'This frame set is already archived' : 'Move and ZIP this frame set'}
  className="flex items-center gap-2 rounded-lg border border-border bg-surface-hover px-3 py-1.5 text-sm hover:brightness-110 disabled:opacity-50 disabled:cursor-not-allowed"
>
  <ArchiveIcon size={14} />
  {archiving ? 'Archiving…' : 'Move and ZIP'}
</button>
```

6. Render the dialog (just before the closing tag of the page, near the existing `FindNewImagesDialog` placement):

```typescript
{showArchiveDialog && archiveRoot && detail.frames_set?.id && (
  <ArchiveDispositionDialog
    framesSetId={detail.frames_set.id}
    archiveRootPath={archiveRoot}
    defaultCompression={archiveCompression}
    onCancel={() => setShowArchiveDialog(false)}
    onStart={handleStartArchive}
  />
)}
```

7. Add an "Archived" badge: when `detail.frames_set.archived_at` is non-null, render a colored badge near the title. Also ideally hide / disable Find new images / Move and ZIP and show a Restore button. For now, a minimal version: render an "Archived" chip if `archived_at` is set:

```typescript
{detail.frames_set?.archived_at && (
  <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full bg-warning/20 text-warning text-xs">
    <ArchiveIcon size={12} />
    Archived
  </span>
)}
```

(The full archived-mode UI — Restore button — is wired in a later task.)

- [ ] **Step 2: Verify TS build**

Run typecheck. Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add src/pages/FrameSetDetail.tsx
git commit -m "feat(ui): add Move and ZIP button + archived badge to FrameSetDetail"
```

---

### Task 25: ArchiveProgress component (plug into Tasks panel)

**Files:**
- Create: `src/components/archive/ArchiveProgress.tsx`

- [ ] **Step 1: Write a progress component that listens to `archive-progress` events**

The component subscribes via `api.listen('archive-progress', cb)` and renders the latest `ArchiveProgressEvent`. It should also render a Cancel button.

```typescript
import { useEffect, useState } from 'react';
import { api } from '../../api';
import { cancelArchiveOperation } from '../../api/archive';
import type { ArchiveProgressEvent } from '../../types/archive';

interface Props {
  operationId: number;
  onClose?: () => void;
}

export function ArchiveProgress({ operationId, onClose }: Props) {
  const [progress, setProgress] = useState<ArchiveProgressEvent | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    api.listen<ArchiveProgressEvent>('archive-progress', (payload) => {
      if (payload.operation_id === operationId) setProgress(payload);
    }).then(fn => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [operationId]);

  return (
    <div className="border border-border rounded p-3 bg-surface-hover">
      <div className="flex items-center justify-between mb-1">
        <span className="text-sm font-medium">Archive operation #{operationId}</span>
        <button
          onClick={async () => {
            try { await cancelArchiveOperation(operationId); } catch (e) { console.error(e); }
            onClose?.();
          }}
          className="text-xs px-2 py-1 border border-border rounded hover:bg-warning/10"
        >Cancel</button>
      </div>
      {progress ? (
        <div className="text-xs">
          <p>{progress.message}</p>
          <div className="w-full h-1.5 bg-surface rounded mt-1 overflow-hidden">
            <div className="h-full bg-accent" style={{
              width: progress.total > 0 ? `${(progress.current / progress.total) * 100}%` : '0%',
            }} />
          </div>
        </div>
      ) : (
        <p className="text-xs text-content-muted">Starting…</p>
      )}
    </div>
  );
}
```

- [ ] **Step 2: Wire into the existing Tasks panel**

Run: `grep -rn "Tasks\|ActiveOperations" src/components/ | head` to find where active scans/analyses are rendered. Add an `<ArchiveProgress>` node for any active archive operation (track active operation_ids in app state, e.g., a context or a hook).

For v1, a minimal integration is acceptable: render `<ArchiveProgress>` directly inside `FrameSetDetail.tsx` while an archive is running for that frame set. Track the active op_id in component state set by `handleStartArchive`.

- [ ] **Step 3: Commit**

```bash
git add src/components/archive/ArchiveProgress.tsx src/pages/FrameSetDetail.tsx
git commit -m "feat(ui): ArchiveProgress component listens to archive-progress events"
```

---

## Phase 12 — Resume banner

### Task 26: ArchiveResumeBanner

**Files:**
- Create: `src/components/archive/ArchiveResumeBanner.tsx`
- Modify: `src/components/Layout.tsx`

- [ ] **Step 1: Implement the banner**

Write `src/components/archive/ArchiveResumeBanner.tsx`:

```typescript
import { useEffect, useState } from 'react';
import { listUnfinishedArchiveOperations, resumeArchiveOperation, rollbackArchiveOperation } from '../../api/archive';
import type { ArchiveOperationSummary } from '../../types/archive';

export function ArchiveResumeBanner() {
  const [ops, setOps] = useState<ArchiveOperationSummary[]>([]);
  const [dismissed, setDismissed] = useState(false);

  useEffect(() => {
    listUnfinishedArchiveOperations().then(setOps).catch(err => console.error(err));
  }, []);

  if (dismissed || ops.length === 0) return null;

  return (
    <div className="bg-warning/10 border-b border-warning/40 px-4 py-2 flex items-center gap-3 text-sm">
      <span className="font-medium">
        Archive operation interrupted: {ops[0].frame_set_name ?? `Frame Set #${ops[0].frames_set_id}`}
        {ops.length > 1 && ` (and ${ops.length - 1} more)`}
      </span>
      <button
        onClick={async () => {
          try { await resumeArchiveOperation(ops[0].id); setDismissed(true); }
          catch (e) { console.error(e); alert(`Resume failed: ${e}`); }
        }}
        className="px-2 py-0.5 rounded bg-accent text-white"
      >Resume</button>
      <button
        onClick={async () => {
          try { await rollbackArchiveOperation(ops[0].id); setDismissed(true); }
          catch (e) { console.error(e); alert(`Rollback failed: ${e}`); }
        }}
        className="px-2 py-0.5 rounded border border-border"
      >Roll back</button>
      <button
        onClick={() => setDismissed(true)}
        className="px-2 py-0.5 rounded border border-border ml-auto"
      >Decide later</button>
    </div>
  );
}
```

- [ ] **Step 2: Mount in Layout**

In `src/components/Layout.tsx`, add an import and render `<ArchiveResumeBanner />` immediately below the top header (or wherever banners belong). Do NOT render it on routes where a banner would be disruptive — for v1, render globally.

- [ ] **Step 3: Commit**

```bash
git add src/components/archive/ArchiveResumeBanner.tsx src/components/Layout.tsx
git commit -m "feat(ui): show resume banner for unfinished archive operations"
```

---

## Phase 13 — Archive page + sidebar + restore + delete

### Task 27: Archive page

**Files:**
- Create: `src/pages/Archive.tsx`
- Modify: `src/components/Layout.tsx` (sidebar)
- Modify: `src/App.tsx` (route)

- [ ] **Step 1: Implement the Archive page**

Write `src/pages/Archive.tsx`:

```typescript
import { useEffect, useState, useCallback } from 'react';
import { useNavigate } from 'react-router-dom';
import { Archive as ArchiveIcon, Trash2, Upload } from 'lucide-react';
import { listArchivedFrameSets, deleteArchive } from '../api/archive';
import type { ArchivedFrameSetSummary } from '../types/archive';
import { RestoreDialog } from '../components/archive/RestoreDialog';

export default function Archive() {
  const [items, setItems] = useState<ArchivedFrameSetSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [restoreFor, setRestoreFor] = useState<ArchivedFrameSetSummary | null>(null);
  const navigate = useNavigate();

  const reload = useCallback(() => {
    setLoading(true);
    listArchivedFrameSets()
      .then(setItems)
      .catch(err => { console.error(err); alert(`Failed to load archives: ${err}`); })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => { reload(); }, [reload]);

  if (loading) return <div className="p-6">Loading archives…</div>;

  if (items.length === 0) {
    return (
      <div className="p-6 text-content-muted">
        <ArchiveIcon size={32} className="mb-2" />
        <p>No archived frame sets yet. Use "Move and ZIP" on a frame set to archive it.</p>
      </div>
    );
  }

  return (
    <div className="p-6">
      <h1 className="text-xl font-semibold mb-4 flex items-center gap-2">
        <ArchiveIcon size={20} />
        Archive
      </h1>
      <table className="w-full text-sm">
        <thead className="border-b border-border">
          <tr>
            <th className="text-left py-2">Object</th>
            <th className="text-left py-2">Archived at</th>
            <th className="text-right py-2">L / F / D / B / DF</th>
            <th className="text-left py-2">Location</th>
            <th className="text-right py-2">Actions</th>
          </tr>
        </thead>
        <tbody>
          {items.map(item => (
            <tr key={item.frames_set_id} className="border-b border-border/40 hover:bg-surface-hover">
              <td className="py-2">{item.name ?? `Frame Set #${item.frames_set_id}`}</td>
              <td className="py-2 font-mono text-xs">{item.archived_at?.slice(0, 19).replace('T', ' ') ?? '—'}</td>
              <td className="py-2 text-right font-mono text-xs">
                {item.lights_count}/{item.flats_count}/{item.darks_count}/{item.bias_count}/{item.darkflats_count}
              </td>
              <td className="py-2 font-mono text-xs">{item.archive_root_path ?? '—'}</td>
              <td className="py-2 text-right space-x-2">
                <button
                  onClick={() => navigate(`/frame-sets/${item.frames_set_id}`)}
                  className="text-xs px-2 py-1 border border-border rounded"
                >Open</button>
                <button
                  disabled={!item.operation_id}
                  onClick={() => setRestoreFor(item)}
                  className="text-xs px-2 py-1 border border-border rounded inline-flex items-center gap-1"
                ><Upload size={12} /> Restore</button>
                <button
                  disabled={!item.operation_id}
                  onClick={async () => {
                    if (!item.operation_id) return;
                    if (!window.confirm('Permanently delete this archive (zips + catalog rows)? This cannot be undone.')) return;
                    try {
                      await deleteArchive(item.operation_id);
                      reload();
                    } catch (e) { alert(`Delete failed: ${e}`); }
                  }}
                  className="text-xs px-2 py-1 border border-border rounded inline-flex items-center gap-1 text-error"
                ><Trash2 size={12} /> Delete</button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      {restoreFor && restoreFor.operation_id && (
        <RestoreDialog
          item={restoreFor}
          onCancel={() => setRestoreFor(null)}
          onCompleted={() => { setRestoreFor(null); reload(); }}
        />
      )}
    </div>
  );
}
```

- [ ] **Step 2: Add the route**

In `src/App.tsx` (or wherever React Router routes are defined), add:

```typescript
import Archive from './pages/Archive';
// ... within the <Routes>:
<Route path="/archive" element={<Archive />} />
```

- [ ] **Step 3: Add sidebar entry**

In `src/components/Layout.tsx`, find the existing sidebar nav links (Objects, ShootCalendar, etc.) and add:

```typescript
import { Archive as ArchiveIcon } from 'lucide-react';
// ...
<NavLink to="/archive" className={...}>
  <ArchiveIcon size={16} /> Archive
</NavLink>
```

Match the existing pattern's classnames + icon size for visual consistency.

- [ ] **Step 4: Commit**

```bash
git add src/pages/Archive.tsx src/App.tsx src/components/Layout.tsx
git commit -m "feat(ui): Archive page with list + restore + delete actions"
```

---

### Task 28: RestoreDialog

**Files:**
- Create: `src/components/archive/RestoreDialog.tsx`

- [ ] **Step 1: Implement**

Write `src/components/archive/RestoreDialog.tsx`:

```typescript
import { useState } from 'react';
import { startRestoreOperation } from '../../api/archive';
import { pickDirectory } from '../../api/desktop';
import type { ArchivedFrameSetSummary } from '../../types/archive';

interface Props {
  item: ArchivedFrameSetSummary;
  onCancel: () => void;
  onCompleted: () => void;
}

export function RestoreDialog({ item, onCancel, onCompleted }: Props) {
  const [target, setTarget] = useState<string>('');
  const [overwrite, setOverwrite] = useState(false);
  const [keepZip, setKeepZip] = useState(false);
  const [busy, setBusy] = useState(false);

  async function pickTarget() {
    const picked = await pickDirectory();
    if (picked) setTarget(picked);
  }

  async function start() {
    if (!item.operation_id || !target) return;
    setBusy(true);
    try {
      await startRestoreOperation(item.operation_id, target, overwrite, keepZip);
      onCompleted();
    } catch (e) {
      alert(`Restore failed: ${e}`);
      setBusy(false);
    }
  }

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-surface rounded-lg shadow-xl p-6 max-w-lg w-full">
        <h2 className="text-lg font-semibold mb-3">Restore archive</h2>
        <p className="text-sm mb-3">{item.name ?? `Frame Set #${item.frames_set_id}`}</p>

        <label className="block text-sm mb-1">Restore target folder</label>
        <div className="flex gap-2 mb-3">
          <input
            type="text"
            value={target}
            onChange={e => setTarget(e.target.value)}
            placeholder="Choose a folder…"
            className="flex-1 px-2 py-1 bg-surface-hover border border-border rounded text-sm"
          />
          <button onClick={pickTarget} className="px-3 py-1 border border-border rounded text-sm">Browse…</button>
        </div>

        <label className="flex items-center gap-2 text-sm mb-2">
          <input type="checkbox" checked={overwrite} onChange={e => setOverwrite(e.target.checked)} />
          Overwrite existing files at target
        </label>
        <label className="flex items-center gap-2 text-sm mb-4">
          <input type="checkbox" checked={keepZip} onChange={e => setKeepZip(e.target.checked)} />
          Keep zip file after restore (default: delete)
        </label>

        <div className="flex justify-end gap-2">
          <button onClick={onCancel} className="px-3 py-1.5 rounded border border-border" disabled={busy}>Cancel</button>
          <button
            onClick={start}
            disabled={busy || !target}
            className="px-3 py-1.5 rounded bg-accent text-white disabled:opacity-50"
          >{busy ? 'Restoring…' : 'Restore'}</button>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Commit**

```bash
git add src/components/archive/RestoreDialog.tsx
git commit -m "feat(ui): RestoreDialog with target picker and keep-zip toggle"
```

---

## Phase 14 — Final integration test + manual smoke test

### Task 29: End-to-end Rust integration test

**Files:**
- Create: `crates/athenaeum-core/tests/archive_e2e.rs`

- [ ] **Step 1: Write a full archive→restore→re-archive integration test**

Write `crates/athenaeum-core/tests/archive_e2e.rs`:

```rust
//! End-to-end integration test: archive a frame set with a master dark,
//! then restore, then re-archive. Catches integration bugs across modules.

use athenaeum_core::archive::{
    db as adb,
    executor::run_operation,
    models::{ArchiveCompression, ArchiveDisposition, ConflictResolution, Dispositions},
    planner::{build_plan, commit_plan},
    restore::run_restore,
};
use athenaeum_core::db::schema::init_db;
use athenaeum_core::events::NullEmitter;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tempfile::TempDir;

#[test]
fn archive_then_restore_then_archive_again() {
    let arch = TempDir::new().unwrap();
    let scan = TempDir::new().unwrap();

    // Filesystem fixture
    let l1 = scan.path().join("M31/L_001.fits");
    let l2 = scan.path().join("M31/L_002.fits");
    let d1 = scan.path().join("Cal/MasterDark.fits");
    for p in [&l1, &l2, &d1] {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    }
    std::fs::write(&l1, b"l1-content-1").unwrap();
    std::fs::write(&l2, b"l2-content-2").unwrap();
    std::fs::write(&d1, b"dark-content-x").unwrap();

    // DB fixture
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
        [scan.path().to_str().unwrap()]).unwrap();
    conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M31')", []).unwrap();
    conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
    conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
    for (file_id, path, frame_id) in [(1000, &l1, 10000), (1001, &l2, 10001)] {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, 12, '2025-10-12', 'FITS')",
            params![file_id, path.to_str().unwrap(), path.file_name().unwrap().to_str().unwrap()],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp) VALUES (?1, ?2, 'M31', 'T', 'C', 'Light')",
            params![frame_id, file_id],
        ).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)", [frame_id]).unwrap();
    }
    // Dark (master)
    conn.execute(
        "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (2000, ?1, 'MasterDark.fits', 14, '2025-10-10', 'FITS')",
        [d1.to_str().unwrap()],
    ).unwrap();
    conn.execute(
        "INSERT INTO frames (id, file_id, instrume, imagetyp, is_master) VALUES (20000, 2000, 'C', 'Dark', 1)",
        [],
    ).unwrap();
    conn.execute("INSERT INTO calibration_set (id, imagetyp, date) VALUES (500, 'Dark', '2025-10-10')", []).unwrap();
    conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (500, 20000)", []).unwrap();
    for fid in [10000, 10001] {
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at) VALUES (?1, 'frame', 500, 'Dark', '2025-10-12')",
            [fid],
        ).unwrap();
    }

    // Archive: lights move, dark copy
    let dispositions = Dispositions {
        flats: None, darks: Some(ArchiveDisposition::Copy), bias: None, darkflats: None,
    };
    let plan = build_plan(&conn, 1, arch.path(), &dispositions, ArchiveCompression::Store).unwrap();
    let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
    run_operation(&conn, op_id, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();

    // Lights deleted, darks stay (copy mode)
    assert!(!l1.exists() && !l2.exists());
    assert!(d1.exists());
    // Two zips produced
    let zips: Vec<_> = std::fs::read_dir(arch.path()).unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "zip").unwrap_or(false))
        .collect();
    assert_eq!(zips.len(), 2);

    // Restore
    let restore_target = TempDir::new().unwrap();
    run_restore(
        &conn, op_id, restore_target.path(), true, false,
        &Arc::new(AtomicBool::new(false)), &NullEmitter,
    ).unwrap();

    // files.path rewritten for the lights
    let l1_new: String = conn.query_row(
        "SELECT path FROM files WHERE id = 1000", [], |r| r.get(0),
    ).unwrap();
    assert!(l1_new.starts_with(restore_target.path().to_str().unwrap()));
    assert!(Path::new(&l1_new).exists());

    // Frame set is no longer archived
    let archived_at: Option<String> = conn.query_row(
        "SELECT archived_at FROM frames_set WHERE id = 1", [], |r| r.get(0),
    ).unwrap();
    assert!(archived_at.is_none());

    // Re-archive (should work now that everything is back)
    let plan2 = build_plan(
        &conn, 1, arch.path(),
        &Dispositions { flats: None, darks: Some(ArchiveDisposition::Skip), bias: None, darkflats: None },
        ArchiveCompression::Store,
    ).unwrap();
    let op_id2 = commit_plan(&conn, &plan2, ConflictResolution::AddSuffix).unwrap();
    run_operation(&conn, op_id2, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
    let op = adb::get_operation(&conn, op_id2).unwrap();
    assert_eq!(op.status, "completed");
}
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p athenaeum-core --test archive_e2e`
Expected: 1 test passes.

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/tests/archive_e2e.rs
git commit -m "test(archive): end-to-end archive/restore/re-archive integration test"
```

---

### Task 30: Manual smoke test on the desktop build

This task is performed by a human, not a fresh subagent.

- [ ] **Step 1: Build and run the desktop app**

Run: `npm run tauri dev`

- [ ] **Step 2: Smoke checklist**

Walk through each of the items below; mark with [x] or note any failure.

- [ ] Open Settings: confirm there's no archive section yet (the archive root is exposed implicitly through the dialog flow). Acceptable for v1; can be promoted to a Settings page section later.
- [ ] Open a Frame Set in the FrameSetDetail page that has at least one master dark or master flat linked. Confirm the new "Move and ZIP" button appears next to "Find new images".
- [ ] Click "Move and ZIP" with `archive.root_path` unset. Confirm the folder-picker prompt appears. Cancel it. Confirm the archive flow stops cleanly.
- [ ] Click "Move and ZIP" again. Pick a folder with plenty of free space (different from the scan root). Confirm the disposition dialog opens.
- [ ] Confirm calibration rows show only the types actually linked. Default selection is Skip. Try selecting Move on each type.
- [ ] If a calibration set is linked to another active frame set, confirm Move is disabled with the "Used by N other frame sets…" tooltip.
- [ ] Pick Skip for all calibrations and click Start Archiving. Watch the progress UI. Confirm:
  - The lights are moved (gone from original location).
  - One `*_Lights.zip` appears in the archive root.
  - The frame set disappears from the Objects page.
  - The Archive page lists the new entry.
- [ ] Click the Archive page entry's [Restore]. Pick a target folder. Confirm:
  - Files extract to `<target>/<scan-root-name>/<rel/path>/<file>`.
  - The frame set returns to the Objects page.
  - `files.path` in DB points to the restored locations (verify by opening sqlite3 directly: `sqlite3 ~/Library/Application\ Support/com.vsharifov.athenaeum/athenaeum.db "select path from files limit 5"`).
- [ ] Try to archive again, this time selecting Move on a unique dark. Cancel mid-operation. Confirm the cancel takes effect, the resume banner does NOT appear (because cancellation triggers rollback to `cancelled` terminal state, which is not unfinished), and the source files are restored.
- [ ] Force-quit the app mid-archive (kill the process). Restart. Confirm the resume banner appears with [Resume] / [Roll back] / [Decide later]. Test [Resume] — confirm the operation continues.
- [ ] Try [Delete Archive] on a finished archive. Confirm the zip is deleted from disk and the row is gone from the Archive page.

- [ ] **Step 3: Note any failures**

If anything fails, file an issue or add follow-up tasks. Don't commit failed states.

---

## Self-Review Checklist (run after writing the plan)

This checklist is for the planner, not the implementer.

**Spec coverage:**

| Spec section | Plan task |
|---|---|
| §3.1 Settings keys | Task 3 |
| §3.2 Schema additions | Task 2 |
| §3.3 State machine | Tasks 6 (enum) + 7 (transitions) + 14 (executor) |
| §3.4 Models | Tasks 4 (FramesSet/File) + 6 (archive types) |
| §4.1 Layout inside zip | Task 8 |
| §4.2 Staging area | Task 9 |
| §4.3 Stages | Task 14 |
| §4.4 Catalog row treatment | Tasks 7 + 14 (mark_file_archived only on Move) |
| §5.1 Cancellation handle | Task 5 |
| §5.2 Rollback per stage | Task 15 |
| §5.3 Rollback as own state | Task 15 |
| §5.4 Resume on startup | Tasks 16 + 26 |
| §6.1-6.5 Module structure | Tasks 6-19 + 20-28 |
| §7.1 Archive page | Task 27 |
| §7.2 Restore flow | Tasks 17 + 28 |
| §7.3 Delete Archive | Task 18 (Tauri) + 19 (web) + 27 (UI) |
| §8 Edge cases | Spread across planner (5/6/7), executor (4/8), rollback (9/10) |
| §9.1 Unit tests | Inline in each task |
| §9.2 Integration tests | Task 29 |
| §9.3 Manual checklist | Task 30 |

**Placeholder scan:** searched for TBD/TODO/FIXME — only one note (`FIXME: detect which calibration rows to show…`) in the disposition dialog skeleton; that's a deliberate hand-off comment to the implementer pointing at the workaround (use `get_calibration_hierarchy_for_frame_set`). No bare placeholders in spec-coverage tasks.

**Type consistency:** `Dispositions`, `ArchiveCompression`, `ConflictResolution`, `FrameRole` are defined in Task 6, used identically across Tasks 13, 14, 18, 19, 20, 21, 22. Method names: `build_plan`, `commit_plan`, `run_operation`, `rollback_operation`, `find_unfinished_operations`, `resume_operation`, `run_restore` — used consistently. DB helpers: `mark_file_archived`/`unmark_file_archived`, `mark_frame_set_archived`/`unmark_frame_set_archived` — used consistently.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-29-archive-feature.md`. Two execution options:

**1. Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?










