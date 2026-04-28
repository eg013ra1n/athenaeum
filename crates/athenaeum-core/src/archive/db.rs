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
