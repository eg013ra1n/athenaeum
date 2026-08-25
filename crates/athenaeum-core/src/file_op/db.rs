//! CRUD for file_operations / file_operation_files / file_operation_steps.

use crate::file_op::models::{
    FileDisposition, FileOpKind, FileOpStage, FileOpStatus, FileOperation, FileOperationFile,
    FileOperationStep, MoveStrategy, StepStatus,
};
use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

/// Insert a new file_operations row. Status starts as Pending.
pub fn insert_operation(
    conn: &Connection,
    kind: FileOpKind,
    source_root: Option<&str>,
    dest_dir: Option<&str>,
) -> Result<i64> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO file_operations (
            kind, status, source_root, dest_dir, total_files, total_bytes, created_at
        ) VALUES (?1, ?2, ?3, ?4, 0, 0, ?5)",
        params![
            kind.as_str(),
            FileOpStatus::Pending.as_str(),
            source_root,
            dest_dir,
            now,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Update the status of an operation. Sets `started_at` on first transition
/// to Running, and `finished_at` on terminal states.
pub fn update_operation_status(
    conn: &Connection,
    operation_id: i64,
    status: FileOpStatus,
    error_message: Option<&str>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let started_at_clause = if matches!(status, FileOpStatus::Running) {
        Some(now.clone())
    } else {
        None
    };
    let is_terminal = matches!(
        status,
        FileOpStatus::Completed | FileOpStatus::Cancelled | FileOpStatus::Failed
    );
    let finished_at_clause = if is_terminal { Some(now) } else { None };
    conn.execute(
        "UPDATE file_operations
         SET status = ?1,
             started_at = COALESCE(started_at, ?2),
             finished_at = COALESCE(?3, finished_at),
             error_message = COALESCE(?4, error_message)
         WHERE id = ?5",
        params![
            status.as_str(),
            started_at_clause,
            finished_at_clause,
            error_message,
            operation_id,
        ],
    )?;
    Ok(())
}

/// Update aggregate counters on the operation row. Called after the planner
/// has committed all the per-file rows.
pub fn update_operation_totals(
    conn: &Connection,
    operation_id: i64,
    total_files: i64,
    total_bytes: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE file_operations SET total_files = ?1, total_bytes = ?2 WHERE id = ?3",
        params![total_files, total_bytes, operation_id],
    )?;
    Ok(())
}

/// Fetch one operation by id.
pub fn get_operation(conn: &Connection, operation_id: i64) -> Result<FileOperation> {
    let op = conn.query_row(
        "SELECT id, kind, status, source_root, dest_dir, total_files, total_bytes,
                created_at, started_at, finished_at, error_message
         FROM file_operations WHERE id = ?1",
        [operation_id],
        |row| {
            Ok(FileOperation {
                id: row.get(0)?,
                kind: row.get(1)?,
                status: row.get(2)?,
                source_root: row.get(3)?,
                dest_dir: row.get(4)?,
                total_files: row.get(5)?,
                total_bytes: row.get(6)?,
                created_at: row.get(7)?,
                started_at: row.get(8)?,
                finished_at: row.get(9)?,
                error_message: row.get(10)?,
            })
        },
    )?;
    Ok(op)
}

/// List operations with status that indicates work is unfinished
/// (Pending or Running). Used at startup to recover from crashes.
pub fn list_unfinished_operations(conn: &Connection) -> Result<Vec<FileOperation>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, status, source_root, dest_dir, total_files, total_bytes,
                created_at, started_at, finished_at, error_message
         FROM file_operations
         WHERE status IN ('pending','running')
         ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(FileOperation {
            id: row.get(0)?,
            kind: row.get(1)?,
            status: row.get(2)?,
            source_root: row.get(3)?,
            dest_dir: row.get(4)?,
            total_files: row.get(5)?,
            total_bytes: row.get(6)?,
            created_at: row.get(7)?,
            started_at: row.get(8)?,
            finished_at: row.get(9)?,
            error_message: row.get(10)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.into())
}

/// Insert a planned file row. Returns its id.
pub fn insert_operation_file(
    conn: &Connection,
    operation_id: i64,
    source_path: &str,
    dest_path: Option<&str>,
    strategy: MoveStrategy,
    catalog_file_id: Option<i64>,
    expected_hash: Option<&str>,
    file_size_bytes: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO file_operation_files (
            operation_id, source_path, dest_path, strategy, catalog_file_id,
            expected_hash, file_size_bytes, disposition
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            operation_id,
            source_path,
            dest_path,
            strategy.as_str(),
            catalog_file_id,
            expected_hash,
            file_size_bytes,
            FileDisposition::Planned.as_str(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Update the disposition of one planned file row.
pub fn update_file_disposition(
    conn: &Connection,
    file_row_id: i64,
    disposition: FileDisposition,
) -> Result<()> {
    conn.execute(
        "UPDATE file_operation_files SET disposition = ?1 WHERE id = ?2",
        params![disposition.as_str(), file_row_id],
    )?;
    Ok(())
}

/// List all planned file rows for an operation, ordered by id (deterministic plan).
pub fn list_operation_files(conn: &Connection, operation_id: i64) -> Result<Vec<FileOperationFile>> {
    let mut stmt = conn.prepare(
        "SELECT id, operation_id, source_path, dest_path, strategy, catalog_file_id,
                expected_hash, file_size_bytes, disposition
         FROM file_operation_files
         WHERE operation_id = ?1
         ORDER BY id",
    )?;
    let rows = stmt.query_map([operation_id], |row| {
        Ok(FileOperationFile {
            id: row.get(0)?,
            operation_id: row.get(1)?,
            source_path: row.get(2)?,
            dest_path: row.get(3)?,
            strategy: row.get(4)?,
            catalog_file_id: row.get(5)?,
            expected_hash: row.get(6)?,
            file_size_bytes: row.get(7)?,
            disposition: row.get(8)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.into())
}

/// Backfill `expected_hash` for a row that degraded from AtomicRename to the
/// cross-volume pipeline at execute time (EXDEV) — rename rows are planned
/// without a hash.
pub fn set_expected_hash(conn: &Connection, file_row_id: i64, hash: &str) -> Result<()> {
    conn.execute(
        "UPDATE file_operation_files SET expected_hash = ?1 WHERE id = ?2",
        params![hash, file_row_id],
    )?;
    Ok(())
}

/// Insert a step row in Pending state. Returns its id.
pub fn insert_step(
    conn: &Connection,
    operation_id: i64,
    operation_file_id: Option<i64>,
    stage: FileOpStage,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO file_operation_steps (
            operation_id, operation_file_id, stage, status
        ) VALUES (?1, ?2, ?3, ?4)",
        params![
            operation_id,
            operation_file_id,
            stage.as_str(),
            StepStatus::Pending.as_str(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Update an existing step row.
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
    let completed_at_clause = if matches!(
        status,
        StepStatus::Done | StepStatus::Failed
    ) {
        Some(now)
    } else {
        None
    };
    conn.execute(
        "UPDATE file_operation_steps
         SET status = ?1,
             actual_hash = COALESCE(?2, actual_hash),
             error_message = COALESCE(?3, error_message),
             started_at = COALESCE(?4, started_at),
             completed_at = COALESCE(?5, completed_at)
         WHERE id = ?6",
        params![
            status.as_str(),
            actual_hash,
            error_message,
            started_at_clause,
            completed_at_clause,
            step_id,
        ],
    )?;
    Ok(())
}

/// Return the set of (operation_file_id) whose Step at `stage` is already Done.
/// Used by the executor to skip work on resume.
pub fn done_file_ids_for_stage(
    conn: &Connection,
    operation_id: i64,
    stage: FileOpStage,
) -> Result<std::collections::HashSet<i64>> {
    let mut stmt = conn.prepare(
        "SELECT operation_file_id
         FROM file_operation_steps
         WHERE operation_id = ?1 AND stage = ?2 AND status = 'done' AND operation_file_id IS NOT NULL",
    )?;
    let rows = stmt.query_map(params![operation_id, stage.as_str()], |row| row.get::<_, i64>(0))?;
    let mut set = std::collections::HashSet::new();
    for r in rows {
        set.insert(r?);
    }
    Ok(set)
}

/// List all steps for an operation, ordered by id.
pub fn list_steps(conn: &Connection, operation_id: i64) -> Result<Vec<FileOperationStep>> {
    let mut stmt = conn.prepare(
        "SELECT id, operation_id, operation_file_id, stage, status, actual_hash, error_message,
                started_at, completed_at
         FROM file_operation_steps
         WHERE operation_id = ?1
         ORDER BY id",
    )?;
    let rows = stmt.query_map([operation_id], |row| {
        Ok(FileOperationStep {
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

/// Hot-sync helper: update the `path` column of a `files` row. Used inside
/// the same transaction as a disk rename or delete-source step.
pub fn update_files_path(
    conn: &Connection,
    file_id: i64,
    new_path: &str,
    new_filename: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE files SET path = ?1, filename = ?2 WHERE id = ?3",
        params![new_path, new_filename, file_id],
    )?;
    Ok(())
}

/// Hot-sync helper: update the `path` (and `filename`) of any catalog row
/// whose current `path` matches `old_path`. Returns the number of rows
/// updated (0 means the source wasn't catalogued, e.g. a sidecar file).
///
/// This is the more robust hot-sync path — it works even when the planner
/// failed to capture a `catalog_file_id` (path-encoding mismatch, etc.) by
/// matching on the current source path inside the executor's transaction.
pub fn update_files_path_by_old_path(
    conn: &Connection,
    old_path: &str,
    new_path: &str,
    new_filename: &str,
) -> Result<usize> {
    let n = conn.execute(
        "UPDATE files SET path = ?1, filename = ?2 WHERE path = ?3",
        params![new_path, new_filename, old_path],
    )?;
    Ok(n)
}

// ============================================================================
// Reconcile support (`file_op::reconcile`)
// ============================================================================

/// One `file_operation_steps` row at stage `commit_move` / status `failed`,
/// joined to its `file_operation_files` row. Produced by
/// `list_failed_commit_move_candidates` and consumed by
/// `file_op::reconcile::reconcile_abandoned_commit_moves` — see that module
/// for the healing rules. Only rows belonging to a `file_operations.kind =
/// 'move'` operation are returned.
#[derive(Debug, Clone)]
pub struct FailedCommitMoveCandidate {
    pub step_id: i64,
    pub operation_id: i64,
    pub operation_file_id: i64,
    pub source_path: String,
    pub dest_path: Option<String>,
    pub expected_hash: Option<String>,
}

/// List every failed CommitMove step belonging to a Move-kind operation,
/// joined to its file row. Ordered by step id for deterministic reconcile
/// order.
pub fn list_failed_commit_move_candidates(conn: &Connection) -> Result<Vec<FailedCommitMoveCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.operation_id, s.operation_file_id,
                f.source_path, f.dest_path, f.expected_hash
         FROM file_operation_steps s
         JOIN file_operation_files f ON f.id = s.operation_file_id
         JOIN file_operations o ON o.id = s.operation_id
         WHERE s.stage = 'commit_move' AND s.status = 'failed' AND o.kind = 'move'
         ORDER BY s.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(FailedCommitMoveCandidate {
            step_id: row.get(0)?,
            operation_id: row.get(1)?,
            operation_file_id: row.get(2)?,
            source_path: row.get(3)?,
            dest_path: row.get(4)?,
            expected_hash: row.get(5)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.into())
}

/// The `actual_hash` of the most recent `Done` Verify step for
/// `(operation_id, operation_file_id)`, or `None` if no such step exists.
/// Reconcile requires this to be present and equal to `expected_hash` before
/// it will trust a re-hash of the destination — a missing/incomplete Verify
/// step means the file was never actually confirmed good.
pub fn verify_step_done_hash(
    conn: &Connection,
    operation_id: i64,
    operation_file_id: i64,
) -> Result<Option<String>> {
    let hash: Option<Option<String>> = conn
        .query_row(
            "SELECT actual_hash FROM file_operation_steps
             WHERE operation_id = ?1 AND operation_file_id = ?2
               AND stage = 'verify' AND status = 'done'
             ORDER BY id DESC LIMIT 1",
            params![operation_id, operation_file_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    Ok(hash.flatten())
}

/// Whether the catalog has a `files` row still pointing at `path`. Used by
/// reconcile to distinguish a leftover-source-only failure (safe to heal)
/// from a case where the hot-sync itself never landed (not safe to heal).
pub fn catalog_row_exists_at_path(conn: &Connection, path: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM files WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

/// True when `operation_id` has at least one `file_operation_files` row and
/// every row's disposition is `Done`. Used by reconcile after healing a file
/// to decide whether the whole operation can be flipped to `Completed`.
pub fn all_operation_files_done(conn: &Connection, operation_id: i64) -> Result<bool> {
    let (total, done): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), SUM(CASE WHEN disposition = 'done' THEN 1 ELSE 0 END)
         FROM file_operation_files WHERE operation_id = ?1",
        params![operation_id],
        |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
    )?;
    Ok(total > 0 && total == done)
}
