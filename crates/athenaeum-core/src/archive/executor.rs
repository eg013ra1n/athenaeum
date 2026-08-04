//! Drive stages 2-7 of an archive operation.

use crate::archive::db as adb;
use crate::archive::models::{
    ArchiveCompression, ArchiveOperationFile, ArchiveStage, ArchiveStatus, StepStatus,
};
use crate::archive::staging;
use crate::archive::zip_reader::verify_zip_contents;
use crate::archive::zip_writer::{build_zip_with_progress, ZipEntry};
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
    let span = tracing::info_span!("archive_op", operation_id);
    let _g = span.enter();

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
    finalize_phase(conn, operation_id, op.frames_set_id, &files, &archive_root, emitter)?;

    adb::update_operation_status(conn, operation_id, ArchiveStatus::Completed, None)?;
    Ok(())
}

/// [`run_operation`] plus the outcome bookkeeping + terminal
/// `archive-finished` event every forward worker needs. Both hosts' start
/// workers call this (and `resume::resume_operation_standalone` the resume
/// workers) so the terminal block exists once — the web worker used to
/// hand-roll it and forgot the emit, leaving the progress widget mounted
/// forever. Same wire shape the desktop worker always emitted:
/// `{operation_id, outcome}` — no `kind`, the frontend defaults to
/// `'archive'`.
pub fn run_operation_standalone(
    conn: &Connection,
    operation_id: i64,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let result = run_operation(conn, operation_id, cancel, emitter);
    finish_forward_operation(conn, operation_id, emitter, result)
}

/// Shared tail of the two standalone entry points: status bookkeeping on
/// error (cancelled/failed + inner rollback), then the terminal event, then
/// the original result so callers can still see the error (already logged
/// here — dropping it at the call site is fine).
pub(crate) fn finish_forward_operation(
    conn: &Connection,
    operation_id: i64,
    emitter: &dyn ProgressEmitter,
    result: Result<()>,
) -> Result<()> {
    let outcome = match &result {
        Ok(()) => {
            tracing::info!(operation_id, "archive operation completed");
            "completed"
        }
        Err(e) => {
            let outcome = if was_cancelled(e) {
                let _ = adb::update_operation_status(conn, operation_id, ArchiveStatus::Cancelled, None);
                "cancelled"
            } else {
                tracing::error!(operation_id, error = ?e, "archive operation failed");
                let msg = format!("{:#}", e);
                let _ = adb::update_operation_status(conn, operation_id, ArchiveStatus::Failed, Some(&msg));
                "failed"
            };
            if let Err(rb_err) = crate::archive::rollback::rollback_operation(conn, operation_id, emitter) {
                tracing::error!(operation_id, error = ?rb_err, "rollback after failed archive operation also failed, operation may be left in an inconsistent state");
            }
            outcome
        }
    };
    emit_event(
        emitter,
        "archive-finished",
        &serde_json::json!({ "operation_id": operation_id, "outcome": outcome }),
    );
    result
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

    let total_files = files.len();
    let existing = existing_done_steps(conn, operation_id, ArchiveStage::ZipAdd)?;

    // Running counter of files processed across all zips in this stage.
    // Includes files in already-Done zips (counted as already-progressed) plus
    // files newly written in this run, so the bar advances smoothly from 0 to
    // total_files regardless of whether the operation is fresh or resumed.
    let mut zipped_so_far: usize = 0;

    for (zip_path_str, group) in by_zip.iter() {
        check_cancel(cancel)?;

        // If every file in this zip already has a Done zip_add step, skip
        // building but still advance the counter so percentage reflects reality.
        let all_done = group.iter().all(|f| existing.contains(&f.id));
        if all_done {
            zipped_so_far += group.len();
            emit_event(emitter, "archive-progress", &ArchiveProgress {
                operation_id,
                stage: "zipping".into(),
                current: zipped_so_far,
                total: total_files,
                message: format!("Skipped (already zipped): {}", zip_path_str),
            });
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

        // Emit a progress event before this zip starts (current count, plus
        // a friendly "Building <filename>" message).
        let zip_filename = zip_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("zip");
        emit_event(emitter, "archive-progress", &ArchiveProgress {
            operation_id,
            stage: "zipping".into(),
            current: zipped_so_far,
            total: total_files,
            message: format!("Building {}", zip_filename),
        });

        let zipped_at_start = zipped_so_far;
        // Per-entry progress callback: called after each entry is fully written.
        let progress_cb = |idx_in_zip: usize, _total_in_zip: usize| {
            let current = zipped_at_start + idx_in_zip;
            emit_event(emitter, "archive-progress", &ArchiveProgress {
                operation_id,
                stage: "zipping".into(),
                current,
                total: total_files,
                message: format!("Building {} ({}/{})", zip_filename, current, total_files),
            });
        };

        match build_zip_with_progress(&zip_path, &entries, compression, Some(&progress_cb)) {
            Ok(_) => {
                for sid in step_ids.into_iter().flatten() {
                    adb::update_step(conn, sid, StepStatus::Done, None, None)?;
                }
                zipped_so_far = zipped_at_start + group.len();
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
    // Total = file count across all zips, so progress is smooth even when
    // there's only one zip with hundreds of entries.
    let total_files = files.len();
    let mut verified_so_far: usize = 0;
    for (zp, expected_entries) in by_zip.iter() {
        check_cancel(cancel)?;
        let zip_name = std::path::Path::new(zp)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("zip");
        emit_event(emitter, "archive-progress", &ArchiveProgress {
            operation_id,
            stage: "zip_verifying".into(),
            current: verified_so_far,
            total: total_files,
            message: format!("Verifying {}", zip_name),
        });
        // Stage-level step (no operation_file_id)
        let sid = adb::insert_step(conn, operation_id, None, ArchiveStage::VerifyZip)?;
        adb::update_step(conn, sid, StepStatus::InProgress, None, None)?;
        match verify_zip_contents(Path::new(zp), expected_entries) {
            Ok(_) => {
                adb::update_step(conn, sid, StepStatus::Done, None, None)?;
                verified_so_far += expected_entries.len();
                emit_event(emitter, "archive-progress", &ArchiveProgress {
                    operation_id,
                    stage: "zip_verifying".into(),
                    current: verified_so_far,
                    total: total_files,
                    message: format!("Verified {} ({}/{})", zip_name, verified_so_far, total_files),
                });
            }
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
            let source_path = Path::new(&f.source_path);
            // If already gone (idempotent resume), treat as success.
            if !source_path.exists() {
                adb::update_step(conn, sid, StepStatus::Done, None, None)?;
            } else {
                match std::fs::remove_file(source_path) {
                    Ok(_) => adb::update_step(conn, sid, StepStatus::Done, None, None)?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        // Raced with another process — file gone, treat as done.
                        adb::update_step(conn, sid, StepStatus::Done, None, None)?;
                    }
                    Err(e) => {
                        let msg = format!("{:#}", e);
                        adb::update_step(conn, sid, StepStatus::Failed, None, Some(&msg))?;
                        anyhow::bail!("delete failed for {}: {}", f.source_path, msg);
                    }
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
///
/// `frames_set_id` is `None` for a calibration-set archive op (Task 14) —
/// there is no frame set to mark, so that step (and its progress tick) is
/// skipped entirely. Per-file marking (`mark_file_archived` below) is
/// subject-agnostic and runs identically either way.
fn finalize_phase(
    conn: &Connection,
    operation_id: i64,
    frames_set_id: Option<i64>,
    files: &[ArchiveOperationFile],
    archive_root: &Path,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    // Total work units: one per moved file (catalog update) + cleanup staging,
    // plus one more for marking the frame set archived when there is one.
    let move_count = files.iter().filter(|f| f.disposition == "move").count();
    let total_units = move_count + if frames_set_id.is_some() { 2 } else { 1 };
    let mut done_units: usize = 0;

    emit_event(emitter, "archive-progress", &ArchiveProgress {
        operation_id,
        stage: "finalizing".into(),
        current: done_units,
        total: total_units,
        message: "Finalizing".into(),
    });
    let sid = adb::insert_step(conn, operation_id, None, ArchiveStage::Finalize)?;
    adb::update_step(conn, sid, StepStatus::InProgress, None, None)?;

    // Mark moved files (one progress tick per moved file)
    for f in files {
        if f.disposition == "move" {
            if let Some(file_id) = f.file_id {
                adb::mark_file_archived(
                    conn, file_id, operation_id, &f.target_zip_path, &f.target_path_in_zip,
                )?;
            }
            done_units += 1;
            emit_event(emitter, "archive-progress", &ArchiveProgress {
                operation_id,
                stage: "finalizing".into(),
                current: done_units,
                total: total_units,
                message: format!("Updating catalog ({}/{})", done_units, total_units),
            });
        }
    }
    if let Some(fs_id) = frames_set_id {
        adb::mark_frame_set_archived(conn, fs_id, operation_id)?;
        done_units += 1;
        emit_event(emitter, "archive-progress", &ArchiveProgress {
            operation_id,
            stage: "finalizing".into(),
            current: done_units,
            total: total_units,
            message: "Marking frame set archived".into(),
        });
    }

    // Cleanup staging.
    // Best-effort: at this point every source is verified inside the zip —
    // a locked staging dir (AV holding a handle) must not mark a functionally
    // complete archive Failed and trigger rollback.
    if let Err(e) = staging::cleanup_staging(archive_root, operation_id) {
        // `{:#}` — cleanup_staging wraps the io error in anyhow context, and
        // plain Display would drop the OS cause ("Access is denied") that is
        // the whole diagnostic value on a sharing violation.
        tracing::warn!(operation_id, error = %format!("{:#}", e), "staging cleanup failed after successful archive; leftover .athenaeum_staging dir");
    }
    adb::update_step(conn, sid, StepStatus::Done, None, None)?;
    done_units += 1;

    // Final 100% event so the bar reaches the end before the worker closes out.
    emit_event(emitter, "archive-progress", &ArchiveProgress {
        operation_id,
        stage: "finalizing".into(),
        current: done_units,
        total: total_units,
        message: "Finalized".into(),
    });
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
    use crate::archive::models::{ConflictResolution, Dispositions};
    use crate::archive::planner::{build_plan, commit_plan};
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use rusqlite::params;
    use std::sync::Mutex;
    use tempfile::TempDir;

    #[derive(Default)]
    struct CapturingEmitter(Mutex<Vec<(String, serde_json::Value)>>);

    impl ProgressEmitter for CapturingEmitter {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((event_name.to_string(), payload));
        }
    }

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
        conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
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

    /// The standalone wrapper owns the terminal event both hosts' workers
    /// relied on hand-rolling (web forgot it; both resume workers forgot it).
    #[test]
    fn run_operation_standalone_emits_completed_terminal_event() {
        let (conn, _arch, _scan, op_id) = run_full_fixture();
        let cancel = Arc::new(AtomicBool::new(false));
        let emitter = CapturingEmitter::default();

        run_operation_standalone(&conn, op_id, &cancel, &emitter).unwrap();

        let events = emitter.0.lock().unwrap();
        let finished: Vec<_> = events.iter().filter(|(n, _)| n == "archive-finished").collect();
        assert_eq!(finished.len(), 1, "exactly one terminal event");
        assert_eq!(finished[0].1["operation_id"], op_id);
        assert_eq!(finished[0].1["outcome"], "completed");
    }

    /// Failure path still emits — and still runs the inner rollback.
    #[test]
    fn run_operation_standalone_emits_failed_for_unknown_operation() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let emitter = CapturingEmitter::default();

        let result = run_operation_standalone(&conn, 999, &cancel, &emitter);
        assert!(result.is_err(), "standalone must still surface the error to its caller");

        let events = emitter.0.lock().unwrap();
        assert!(
            events.iter().any(|(n, p)| n == "archive-finished" && p["outcome"] == "failed"),
            "terminal event must fire on failure too"
        );
    }

    /// The third outcome of the shared terminal tail. A cancel is NOT a
    /// failure: `finish_forward_operation` must emit `outcome: "cancelled"`, so
    /// the progress widget dismisses in the cancel colour rather than reporting
    /// an error to the user, and must still roll the partial work back.
    #[test]
    fn run_operation_standalone_emits_cancelled_terminal_event() {
        let (conn, _arch, _scan, op_id) = run_full_fixture();
        let cancel = Arc::new(AtomicBool::new(true)); // pre-cancelled
        let emitter = CapturingEmitter::default();

        let result = run_operation_standalone(&conn, op_id, &cancel, &emitter);
        assert!(result.is_err(), "standalone must still surface the cancel to its caller");

        let events = emitter.0.lock().unwrap();
        let finished: Vec<_> = events.iter().filter(|(n, _)| n == "archive-finished").collect();
        assert_eq!(finished.len(), 1, "exactly one terminal event");
        assert_eq!(finished[0].1["operation_id"], op_id);
        assert_eq!(
            finished[0].1["outcome"], "cancelled",
            "a cancel must not be reported as a failure"
        );

        // The row lands on `rolled_back`, not `cancelled`: the cancel branch
        // stamps `Cancelled` and then runs the same rollback the failure branch
        // does (restore sources, delete partial zips), and rollback stamps
        // `RolledBack` last. That ordering is the point — a cancelled operation
        // must not leave partial work behind — so this pins the terminal state
        // rather than the transient one.
        let status: String = conn
            .query_row("SELECT status FROM archive_operations WHERE id = ?1", [op_id], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "rolled_back", "cancel rolls the partial operation back");
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
