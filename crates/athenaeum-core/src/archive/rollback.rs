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
    ArchiveStage, ArchiveStatus, StepStatus,
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
                // Best-effort by design — a locked zip (Windows sharing
                // violation) must not fail the rollback — but never silent.
                if let Err(e) = std::fs::remove_file(zp) {
                    tracing::warn!(
                        operation_id,
                        path = %zp.display(),
                        error = %e,
                        "failed to remove partial zip during rollback"
                    );
                }
            }
        }
    }

    // 3. Delete staging dir.
    let sid = adb::insert_step(conn, operation_id, None, ArchiveStage::DeleteStaging)?;
    adb::update_step(conn, sid, StepStatus::InProgress, None, None)?;
    staging::cleanup_staging(&archive_root, operation_id)?;
    adb::update_step(conn, sid, StepStatus::Done, None, None)?;

    // 4. Clear zip markers on the frame set, but keep is_archived as-is —
    //    the frame set was in the Archive section before the failed op started.
    //    A calibration-set op (Task 14) has no frame set to clear — skip.
    if let Some(fs_id) = op.frames_set_id {
        adb::clear_zip_markers(conn, fs_id)?;
    }
    for f in &files {
        if let Some(file_id) = f.file_id {
            adb::unmark_file_archived(conn, file_id, None, None, None)?;
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
    use crate::archive::models::{ArchiveCompression, ConflictResolution, Dispositions};
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
        conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        for (file_id, path, frame_id) in [(1000i64, &l1, 10000i64), (1001, &l2, 10001)] {
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
