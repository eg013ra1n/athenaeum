//! Archive commands both hosts wrap: the three listings the Archive page reads
//! (archived frame sets, an operation's zips, restore suggestions), the
//! restore worker and the deletion of an archive. The listings return ad-hoc
//! JSON with the keys the frontend reads, so the wire shape does not change.

use crate::api::{db, ApiError};
use crate::archive::models::{ArchiveCompression, ConflictResolution, Dispositions};
use crate::archive::{db as adb, executor, planner, restore};
use crate::events::ProgressEmitter;
use crate::services::operation_queue::{OperationKind, QueuedJob};
use crate::services::{ArchiveHandle, ServiceContext};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Every frame set that has been archived, with the per-role file counts of the
/// operation that archived it.
pub fn list_archived_frame_sets(ctx: &ServiceContext) -> Result<Vec<serde_json::Value>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let mut stmt = conn
        .prepare(
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
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let rows: Vec<serde_json::Value> = stmt
        .query_map([], |row| {
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
        })
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(rows)
}

/// The zips of one archive operation, lights first, each with a disk stat.
pub fn list_archive_zips(
    ctx: &ServiceContext,
    operation_id: i64,
) -> Result<Vec<serde_json::Value>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    // Unique target_zip_paths for the operation, ordered by frame_role priority.
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT target_zip_path, frame_role
             FROM archive_operation_files
             WHERE operation_id = ?1
             ORDER BY
                CASE frame_role
                    WHEN 'light' THEN 0
                    WHEN 'flat'  THEN 1
                    WHEN 'darkflat' THEN 2
                    WHEN 'dark'  THEN 3
                    WHEN 'bias'  THEN 4
                    ELSE 5
                END,
                target_zip_path",
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let rows = stmt
        .query_map([operation_id], |row| {
            let path: String = row.get(0)?;
            let frame_role: String = row.get(1)?;
            let p = Path::new(&path);
            let filename = p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let exists = p.is_file();
            let size_bytes = std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0);
            Ok(serde_json::json!({
                "path": path,
                "filename": filename,
                "frame_role": frame_role,
                "exists": exists,
                "size_bytes": size_bytes,
            }))
        })
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(rows)
}

/// Where an archive would be restored to: the original parent directory, and
/// the scan roots to pick from.
pub fn get_restore_suggestions(
    ctx: &ServiceContext,
    operation_id: i64,
) -> Result<serde_json::Value, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    // Take the first operation file and strip the target_path_in_zip from
    // the source_path; what remains is the original parent directory.
    // All files in a single operation share the same parent (since prefixes
    // are scan-root-relative).
    let suggested: Option<String> = conn
        .query_row(
            "SELECT source_path, target_path_in_zip
             FROM archive_operation_files
             WHERE operation_id = ?1
             LIMIT 1",
            [operation_id],
            |row| {
                let source_path: String = row.get(0)?;
                let path_in_zip: String = row.get(1)?;
                Ok(crate::archive::path_layout::original_parent_for_restore(
                    &source_path,
                    &path_in_zip,
                ))
            },
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Existing scan roots
    let mut stmt = conn
        .prepare("SELECT path FROM scan_roots ORDER BY path")
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let scan_roots: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Whether the suggested original path still exists and is writable.
    let suggested_exists = suggested
        .as_ref()
        .map(|p| Path::new(p).is_dir())
        .unwrap_or(false);

    Ok(serde_json::json!({
        "suggested_original_path": suggested,
        "suggested_original_exists": suggested_exists,
        "scan_roots": scan_roots,
    }))
}

/// Plan and commit an archive of one frame set, then queue it on the shared
/// disk worker. The plan is built and committed synchronously; returns the
/// operation id once the job is queued. The worker runs the operation (outcome
/// bookkeeping and the terminal `archive-finished` event live in
/// `executor::run_operation_standalone`) and drops the handle.
///
/// `refuse_when_busy`: the two hosts differ today. The web passes `true` and is
/// refused (`Conflict`) while `ctx.active_archives` is not empty; the desktop
/// passes `false` and the job queues behind the running one. `active_archives`
/// also holds file operations and master builds, so the guard refuses during
/// those too. Whether the desktop should refuse is the owner's open decision.
#[allow(clippy::too_many_arguments)]
pub fn start_archive_operation(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    frames_set_id: i64,
    dispositions: &Dispositions,
    compression: ArchiveCompression,
    conflict_resolution: ConflictResolution,
    archive_root_path: Option<&str>,
    refuse_when_busy: bool,
) -> Result<i64, ApiError> {
    if refuse_when_busy {
        let map = ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err(ApiError::Conflict(
                "another archive operation is already in progress".into(),
            ));
        }
    }
    let op_id = {
        let db = db(&ctx)?;
        let conn = db.conn();
        let root = crate::archive::resolve_archive_root(&conn, &ctx.settings, archive_root_path)
            .map_err(|e| ApiError::Invalid(format!("{:#}", e)))?;

        // Build + commit the plan synchronously.
        let plan = planner::build_plan(
            &conn,
            frames_set_id,
            Path::new(&root),
            dispositions,
            compression,
        )
        .map_err(|e| ApiError::Internal(format!("{:#}", e)))?;
        planner::commit_plan(&conn, &plan, conflict_resolution)
            .map_err(|e| ApiError::Internal(format!("{:#}", e)))?
    };

    // Register the cancel flag so cancel_archive_operation can find it.
    let cancel_flag = Arc::new(AtomicBool::new(false));
    ctx.active_archives.lock().unwrap().insert(
        op_id,
        ArchiveHandle {
            operation_id: op_id,
            cancel_flag: cancel_flag.clone(),
        },
    );

    // Enqueue. Single global worker (shared with file ops) drives execution
    // serially. The DB connection is obtained inside the closure since
    // rusqlite connections are not Send across thread boundaries.
    let ctx_for_worker = ctx.clone();
    ctx.operation_queue.enqueue(QueuedJob {
        kind: OperationKind::ZipArchive,
        operation_id: op_id,
        run: Box::new(move || {
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();
            // Outcome bookkeeping + terminal `archive-finished` live in core
            // (`run_operation_standalone`). Errors are logged (and rolled
            // back) inside; nothing to add here.
            let _ = executor::run_operation_standalone(&conn, op_id, &cancel_flag, emitter.as_ref());
            ctx_for_worker.active_archives.lock().unwrap().remove(&op_id);
        }),
    });

    Ok(op_id)
}

/// Queue a restore of an archive operation on the shared disk worker. Refuses
/// while any archive operation is active; returns once the job is queued. The
/// worker maps the restore outcome and emits the terminal `archive-finished`
/// event.
pub fn start_restore_operation(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    operation_id: i64,
    target_root_path: String,
    overwrite_existing: bool,
    keep_zip_after_restore: bool,
) -> Result<(), ApiError> {
    {
        let map = ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err(ApiError::Conflict(
                "another archive operation is already in progress".into(),
            ));
        }
    }
    let cancel_flag = Arc::new(AtomicBool::new(false));
    ctx.active_archives.lock().unwrap().insert(
        operation_id,
        ArchiveHandle {
            operation_id,
            cancel_flag: cancel_flag.clone(),
        },
    );
    let ctx_for_worker = ctx.clone();
    ctx.operation_queue.enqueue(QueuedJob {
        kind: OperationKind::ZipArchive,
        operation_id,
        run: Box::new(move || {
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();
            let (outcome, conflicts) = match restore::run_restore(
                &conn,
                operation_id,
                Path::new(&target_root_path),
                overwrite_existing,
                keep_zip_after_restore,
                &cancel_flag,
                emitter.as_ref(),
            ) {
                Ok(result) if result.has_conflicts() => {
                    tracing::warn!(
                        operation_id,
                        conflicts = result.conflicts.len(),
                        paths = ?result.conflicts.iter().map(|c| c.source_path.as_str()).collect::<Vec<_>>(),
                        "restore completed with conflicts"
                    );
                    ("completed_with_conflicts", result.conflicts.len())
                }
                Ok(_) => ("completed", 0),
                Err(e) => {
                    tracing::error!(operation_id, error = ?e, "restore failed");
                    let outcome = if format!("{:#}", e).contains("cancelled") {
                        "cancelled"
                    } else {
                        "failed"
                    };
                    (outcome, 0)
                }
            };
            crate::events::emit_event(
                emitter.as_ref(),
                "archive-finished",
                &serde_json::json!({
                    "operation_id": operation_id,
                    "outcome": outcome,
                    "kind": "restore",
                    "conflicts": conflicts,
                }),
            );
            ctx_for_worker.active_archives.lock().unwrap().remove(&operation_id);
        }),
    });
    Ok(())
}

/// Delete an archive: its zip files, then its catalog rows. No transaction: the
/// zips go first one by one and any failure aborts before a catalog row is
/// touched.
pub fn delete_archive(ctx: &ServiceContext, operation_id: i64) -> Result<(), ApiError> {
    {
        let map = ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err(ApiError::Conflict(
                "another archive operation is already in progress".into(),
            ));
        }
    }
    let db = db(ctx)?;
    let conn = db.conn();

    // Delete zip files. A zip we could not delete (Windows sharing violation,
    // read-only volume) must abort the whole command BEFORE any catalog row
    // goes away — otherwise the zip is orphaned on disk with nothing pointing
    // at it and the archive becomes unrestorable.
    let files = adb::list_operation_files(&conn, operation_id)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut seen = std::collections::HashSet::new();
    let mut failed: Vec<String> = Vec::new();
    for f in &files {
        if seen.insert(f.target_zip_path.clone()) {
            let p = Path::new(&f.target_zip_path);
            if p.exists() {
                if let Err(e) = std::fs::remove_file(p) {
                    tracing::error!(operation_id, path = %f.target_zip_path, error = %e, "failed to delete archive zip");
                    failed.push(format!("{}: {}", f.target_zip_path, e));
                }
            }
        }
    }
    if !failed.is_empty() {
        return Err(ApiError::Internal(format!(
            "could not delete {} zip file(s); catalog rows kept — any zip already removed in this pass is not restorable: {}",
            failed.len(),
            failed.join("; ")
        )));
    }

    // Get frames_set_id, then delete frame set + cascading rows.
    let op = adb::get_operation(&conn, operation_id)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    match op.frames_set_id {
        Some(fs_id) => {
            conn.execute("DELETE FROM frames_set WHERE id = ?1", [fs_id])
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            // archive_operations row is also deleted via FK cascade from frames_set_id.
        }
        None => {
            // Calibration-set archive op (Task 14): no frames_set to cascade
            // from — delete the archive_operations row directly (cascades to
            // archive_operation_files/steps via their own FKs).
            conn.execute("DELETE FROM archive_operations WHERE id = ?1", [operation_id])
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::NullEmitter;

    fn test_ctx() -> (tempfile::TempDir, Arc<ServiceContext>) {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        (tmp, Arc::new(ctx))
    }

    /// A calibration-set style archive (no frame set): the zip goes off the
    /// disk and the operation row (with its files) goes from the catalog.
    #[test]
    fn delete_archive_removes_the_zip_and_the_catalog_rows() {
        let (tmp, ctx) = test_ctx();
        let zip = tmp.path().join("light.zip");
        std::fs::write(&zip, b"zip").unwrap();
        let op_id = {
            let conn = ctx.db.get().unwrap().conn();
            let op_id =
                adb::insert_operation(&conn, None, None, "/archive", None, None, None, None, "store")
                    .unwrap();
            adb::insert_operation_file(
                &conn,
                op_id,
                None,
                "/data/M31/a.fits",
                zip.to_str().unwrap(),
                "M31/a.fits",
                "h",
                "move",
                "light",
                3,
            )
            .unwrap();
            op_id
        };

        delete_archive(&ctx, op_id).expect("delete archive");

        assert!(!zip.exists());
        let conn = ctx.db.get().unwrap().conn();
        let ops: i64 = conn
            .query_row("SELECT COUNT(*) FROM archive_operations", [], |r| r.get(0))
            .unwrap();
        let files: i64 = conn
            .query_row("SELECT COUNT(*) FROM archive_operation_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!((ops, files), (0, 0));
    }

    /// A restore is refused while any archive operation holds a handle, and
    /// the refusal registers nothing.
    #[test]
    fn start_restore_operation_is_refused_while_an_archive_is_active() {
        let (_tmp, ctx) = test_ctx();
        ctx.active_archives.lock().unwrap().insert(
            7,
            ArchiveHandle {
                operation_id: 7,
                cancel_flag: Arc::new(AtomicBool::new(false)),
            },
        );

        let err = start_restore_operation(
            ctx.clone(),
            Arc::new(NullEmitter),
            9,
            "/restore".into(),
            false,
            false,
        )
        .expect_err("busy");

        assert!(matches!(err, ApiError::Conflict(_)));
        assert_eq!(err.to_string(), "another archive operation is already in progress");
        assert_eq!(ctx.active_archives.lock().unwrap().len(), 1);
    }

    /// `refuse_when_busy` refuses while any archive operation holds a handle;
    /// the same call without it is not stopped by the guard.
    #[test]
    fn start_archive_operation_guard_follows_refuse_when_busy() {
        let (_tmp, ctx) = test_ctx();
        ctx.active_archives.lock().unwrap().insert(
            7,
            ArchiveHandle {
                operation_id: 7,
                cancel_flag: Arc::new(AtomicBool::new(false)),
            },
        );
        let start = |refuse| {
            start_archive_operation(
                ctx.clone(),
                Arc::new(NullEmitter),
                1,
                &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
                ArchiveCompression::Store,
                ConflictResolution::AddSuffix,
                Some("/archive"),
                refuse,
            )
        };

        let err = start(true).expect_err("busy");
        assert!(matches!(err, ApiError::Conflict(_)));
        assert_eq!(err.to_string(), "another archive operation is already in progress");

        if let Err(e) = start(false) {
            assert_ne!(e.to_string(), "another archive operation is already in progress");
        }
        assert!(ctx.active_archives.lock().unwrap().contains_key(&7));
    }
}
