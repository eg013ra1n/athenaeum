//! Drive stages 2-7 of an archive operation.

use crate::archive::db as adb;
use crate::archive::models::{
    ArchiveCompression, ArchiveOperationFile, ArchiveStage, ArchiveStatus, StepStatus,
};
use crate::archive::staging;
use crate::archive::zip_reader::verify_zip_contents;
use crate::archive::zip_writer::{build_zip_partial, sweep_stale_partials_beside, ZipEntry};
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
/// The caller (run_operation) catches this, leaves the status as the phase
/// wrote it, and lets rollback take over. We use the message string rather than a bespoke type
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
/// On cancel or error: the status is left as the running phase set it (no Failed/Cancelled is written),
/// the error is kept on the row, and the caller invokes rollback, which ends it at `rolled_back`. A refused
/// rollback leaves the phase status, so the operation stays listed as unfinished (Resume / Roll back).
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
/// error (the error is kept on the row, then the inner rollback; no Failed/Cancelled status is written), then the terminal event, then
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
        Err(e) => fail_and_roll_back(conn, operation_id, emitter, e),
    };
    emit_event(
        emitter,
        "archive-finished",
        &serde_json::json!({ "operation_id": operation_id, "outcome": outcome }),
    );
    result
}

/// The failure path of a forward operation, shared by every worker that runs [`run_operation`]
/// (`finish_forward_operation`, the calibration-archive worker in `api::masters`): note the error on the
/// row, then roll back, and return the outcome name for the terminal event (`"cancelled"` / `"failed"`).
///
/// The row's status is NOT switched to `failed` / `cancelled` first. A rollback that refuses before it
/// touches anything (the only copy of a frame would be lost) must leave the operation in the in-progress
/// status it died in: `list_unfinished_operations` then offers Resume / Roll back again, where a `failed`
/// row would be invisible to the banner. A rollback that completes ends the row at `rolled_back`, the
/// terminal state both orderings reached before.
pub fn fail_and_roll_back(
    conn: &Connection,
    operation_id: i64,
    emitter: &dyn ProgressEmitter,
    err: &anyhow::Error,
) -> &'static str {
    let outcome = if was_cancelled(err) {
        "cancelled"
    } else {
        tracing::error!(operation_id, error = ?err, "archive operation failed");
        let msg = format!("{:#}", err);
        if let Err(e) = conn.execute(
            "UPDATE archive_operations SET error_message = ?1 WHERE id = ?2",
            rusqlite::params![msg, operation_id],
        ) {
            tracing::error!(operation_id, error = ?e, "could not record the failure on the archive operation");
        }
        "failed"
    };
    if let Err(rb_err) = crate::archive::rollback::rollback_operation(conn, operation_id, emitter) {
        tracing::error!(
            operation_id,
            error = ?rb_err,
            "rollback after failed archive operation also failed; the operation keeps its in-progress status and can be resumed or rolled back again"
        );
    }
    outcome
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

        // Never truncate a zip that catalog rows live in (checked again here: the plan may be old).
        crate::archive::planner::ensure_zip_holds_no_cataloged_frames(conn, zip_path_str)?;

        // Make sure parent dir exists.
        let zip_path = PathBuf::from(zip_path_str);
        if let Some(parent) = zip_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create zip dir {}", parent.display()))?;
        }
        // A build that died with its process left a partial here: the resumed run must not leave it behind
        // next to the zip it is about to publish. This process's own id counts as stale too (a container
        // runs as pid 1 every time, so a crashed run's partial carries the id of the run that resumes).
        sweep_stale_partials_beside(&zip_path, true);

        // Build the zip from staging files.
        let entries: Vec<ZipEntry> = group.iter().map(|f| ZipEntry {
            source_path: staging::staging_file_path(archive_root, operation_id, &f.target_path_in_zip),
            path_in_zip: f.target_path_in_zip.clone(),
        }).collect();

        // Insert one InProgress step per file in this group; flip them to Done once the zip is in place.
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

        // The zip is built in a partial beside the target and verified there; only a complete, verified zip
        // is renamed onto the target. Until the rename the target (an old zip being overwritten) is
        // untouched, and any failure before it removes the partial and leaves the target as it was.
        let expected: Vec<String> = group.iter().map(|f| f.target_path_in_zip.clone()).collect();
        let built = (|| -> Result<()> {
            let partial = build_zip_partial(&zip_path, &entries, compression, Some(&progress_cb))?;
            verify_zip_contents(partial.path(), &expected)
                .context("the built zip failed verification, so it was not moved into place")?;
            partial.publish(&zip_path)
        })();
        match built {
            Ok(()) => {
                // The rename is done: from here rollback may remove the target. Recorded before anything else.
                let pid = adb::insert_step(conn, operation_id, Some(group[0].id), ArchiveStage::ZipPublish)?;
                adb::update_step(conn, pid, StepStatus::Done, None, None)?;
                for sid in step_ids.into_iter().flatten() {
                    adb::update_step(conn, sid, StepStatus::Done, None, None)?;
                }
                zipped_so_far = zipped_at_start + group.len();
            }
            Err(e) => {
                let msg = format!("{:#}", e);
                tracing::error!(operation_id, zip_path = %zip_path_str, error = %msg, "zip build failed; the target is untouched");
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

    // The CRC32 the zip's directory holds for each entry: one directory read per built zip, before
    // any row changes, so a zip that cannot be read fails the finalize with the catalog untouched.
    // The unfiltered by-name map is used (not the scan listing) because the entries our own layout
    // writes may carry a `:` or a trailing space that the scan listing leaves out.
    let mut crcs_by_zip: HashMap<&str, HashMap<String, u32>> = HashMap::new();
    for f in files {
        if crcs_by_zip.contains_key(f.target_zip_path.as_str()) {
            continue;
        }
        let crcs = crate::zip_store::reader::entry_crc32_map(Path::new(&f.target_zip_path)).map_err(|e| {
            tracing::error!(operation_id, zip_path = %f.target_zip_path, error = %format!("{e:#}"), "reading the built zip's directory failed");
            e.context(format!("read the directory of {}", f.target_zip_path))
        })?;
        crcs_by_zip.insert(f.target_zip_path.as_str(), crcs);
    }

    // Mark moved files (one progress tick per moved file)
    for f in files {
        if f.disposition == "move" {
            if let Some(file_id) = f.file_id {
                adb::mark_file_archived(
                    conn, file_id, operation_id, &f.target_zip_path, &f.target_path_in_zip,
                )?;
                let crc = crcs_by_zip
                    .get(f.target_zip_path.as_str())
                    .and_then(|m| m.get(&f.target_path_in_zip))
                    .copied()
                    .ok_or_else(|| {
                        tracing::error!(operation_id, file_id, zip_path = %f.target_zip_path, zip_entry = %f.target_path_in_zip, "built zip has no entry for a moved file");
                        anyhow::anyhow!("{} has no entry {}", f.target_zip_path, f.target_path_in_zip)
                    })?;
                adb::set_file_entry_crc32(conn, file_id, crc)?;
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
    // Each built zip joins the zip catalog with its real stat, so a later scan of it is skipped as
    // unchanged and its created-by operation is on record.
    for zip_path in crcs_by_zip.keys() {
        // Frame entries the operation wrote into this zip: moves and copies alike (copies have no row).
        let written = files.iter().filter(|f| f.target_zip_path == *zip_path).count();
        record_built_zip(conn, operation_id, zip_path, written)?;
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

/// Upsert the `zip_archives` row of a zip this operation built: the zip's real size and mtime (in the
/// format the zip scan writes, so the scan finds them equal), the number of frame entries the operation
/// wrote into it (`frame_entries`) and `created_by_operation`. Entries an earlier unpack released from
/// a zip at this path no longer apply to the zip just built, so they are cleared.
fn record_built_zip(conn: &Connection, operation_id: i64, zip_path: &str, frame_entries: usize) -> Result<()> {
    let meta = std::fs::metadata(zip_path).map_err(|e| {
        tracing::error!(operation_id, zip_path, error = %e, "stat of the built zip failed");
        anyhow::Error::new(e).context(format!("stat {zip_path}"))
    })?;
    let modified = meta.modified().map_err(|e| {
        tracing::error!(operation_id, zip_path, error = %e, "mtime of the built zip unreadable");
        anyhow::Error::new(e).context(format!("mtime of {zip_path}"))
    })?;
    let modified = chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339();
    conn.execute(
        "INSERT INTO zip_archives (path, size, modified_at, frame_entries, scanned_at, created_by_operation)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(path) DO UPDATE SET size = excluded.size, modified_at = excluded.modified_at,
                frame_entries = excluded.frame_entries, scanned_at = excluded.scanned_at,
                created_by_operation = excluded.created_by_operation",
        rusqlite::params![
            zip_path,
            meta.len() as i64,
            modified,
            frame_entries as i64,
            chrono::Utc::now().to_rfc3339(),
            operation_id
        ],
    )?;
    conn.execute(
        "DELETE FROM zip_released_entries WHERE zip_id = (SELECT id FROM zip_archives WHERE path = ?1)",
        rusqlite::params![zip_path],
    )?;
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

/// Single forward phases for the tests of other archive modules (rollback), so they can stop an
/// operation exactly where an interruption would.
#[cfg(test)]
pub(crate) mod test_phases {
    use super::*;

    pub(crate) fn copy_only(
        conn: &Connection,
        op: i64,
        files: &[ArchiveOperationFile],
        root: &Path,
        cancel: &CancelFlag,
    ) -> Result<()> {
        copy_phase(conn, op, files, root, cancel, &crate::events::NullEmitter)?;
        verify_copy_phase(conn, op, files, root, cancel, &crate::events::NullEmitter)
    }

    pub(crate) fn zip_only(
        conn: &Connection,
        op: i64,
        files: &[ArchiveOperationFile],
        root: &Path,
        cancel: &CancelFlag,
    ) -> Result<()> {
        zip_phase(conn, op, files, root, ArchiveCompression::Store, cancel, &crate::events::NullEmitter)
    }

    pub(crate) fn copy_and_zip(
        conn: &Connection,
        op: i64,
        files: &[ArchiveOperationFile],
        root: &Path,
        cancel: &CancelFlag,
    ) -> Result<()> {
        copy_only(conn, op, files, root, cancel)?;
        zip_only(conn, op, files, root, cancel)
    }
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
        full_fixture_with(&["M31/L_001.fits", "M31/L_002.fits"])
    }

    /// [`run_full_fixture`] for lights at `rels` (relative to the scan root). The names may carry `:`, a
    /// trailing space or a device name in a folder, as object and folder names do.
    fn full_fixture_with(rels: &[&str]) -> (Connection, TempDir, TempDir, i64) {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();

        let lights: Vec<PathBuf> = rels.iter().map(|r| scan.path().join(r)).collect();
        for (i, l) in lights.iter().enumerate() {
            std::fs::create_dir_all(l.parent().unwrap()).unwrap();
            std::fs::write(l, format!("light-{}", i + 1)).unwrap();
        }

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
        for (i, path) in lights.iter().enumerate() {
            let (file_id, frame_id) = (1000 + i as i64, 10000 + i as i64);
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
        // writes no status of its own and runs the same rollback the failure branch
        // does (restore sources, delete partial zips), and rollback stamps
        // `RolledBack` last (a refused rollback would leave the phase status). That ordering is the point — a cancelled operation
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

    /// `(zip path, size, modified_at, frame_entries, created_by_operation)` of every `zip_archives` row.
    fn zip_rows(conn: &Connection) -> Vec<(String, i64, String, i64, Option<i64>)> {
        let mut stmt = conn
            .prepare("SELECT path, size, modified_at, frame_entries, created_by_operation FROM zip_archives ORDER BY path")
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows
    }

    #[test]
    fn finalize_records_the_zip_and_entry_crcs() {
        let (conn, _arch, _scan, op_id) = run_full_fixture();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();

        let zips = zip_rows(&conn);
        assert_eq!(zips.len(), 1);
        let (path, size, modified, entries, op) = &zips[0];
        let meta = std::fs::metadata(path).unwrap();
        assert_eq!(*size, meta.len() as i64, "the real size, not a sentinel");
        let expected_modified = chrono::DateTime::<chrono::Utc>::from(meta.modified().unwrap()).to_rfc3339();
        assert_eq!(modified, &expected_modified, "the format the zip scan writes");
        assert_eq!(*entries, 2);
        assert_eq!(*op, Some(op_id));

        let crcs = crate::zip_store::reader::entry_crc32_map(Path::new(path)).unwrap();
        let rows: Vec<(String, Option<i64>)> = {
            let mut stmt = conn
                .prepare("SELECT archive_path_in_zip, archive_entry_crc32 FROM files WHERE archived_in_operation = ?1")
                .unwrap();
            let r = stmt
                .query_map([op_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            r
        };
        assert_eq!(rows.len(), 2);
        for (entry, crc) in rows {
            assert_eq!(crc, Some(i64::from(crcs[&entry])), "{entry}");
        }
    }

    #[test]
    fn rescan_of_our_archive_zip_adds_no_rows() {
        let (conn, _arch, _scan, op_id) = run_full_fixture();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();
        let before = zip_rows(&conn);
        let zip = PathBuf::from(&before[0].0);
        let files_before: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();

        // The recorded stat equals the zip's: the scan opens nothing.
        let outcome = crate::zip_store::scan::scan_zip(&conn, &zip, 1, false).unwrap();
        assert!(outcome.skipped_unchanged);
        assert!(outcome.inserted.is_empty());
        assert_eq!(outcome.files_inserted, 0);

        // Even a scan that does read the zip finds every entry owned by a row, adds none, and leaves
        // the operation on record and the real stat in place.
        conn.execute("UPDATE zip_archives SET size = 0, modified_at = ''", []).unwrap();
        let outcome = crate::zip_store::scan::scan_zip(&conn, &zip, 1, false).unwrap();
        assert!(!outcome.skipped_unchanged);
        assert!(outcome.inserted.is_empty());
        assert_eq!(outcome.files_inserted, 0);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        let files_after: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(files_after, files_before);
        let after = zip_rows(&conn);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].1, before[0].1, "the real size is back");
        assert_eq!(after[0].2, before[0].2);
        assert_eq!(after[0].3, 2);
        assert_eq!(after[0].4, Some(op_id), "created_by_operation survives the rescan");
    }

    #[test]
    fn a_zip_of_copies_only_is_recorded_with_its_entries_and_a_rescan_adds_no_rows() {
        let (conn, _arch, _scan, op_id) = run_full_fixture();
        // Copy darks / library masters have no row: the original on disk stays the cataloged frame.
        conn.execute("UPDATE archive_operation_files SET disposition = 'copy' WHERE operation_id = ?1", [op_id]).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();

        let zips = zip_rows(&conn);
        assert_eq!(zips.len(), 1);
        assert_eq!(zips[0].3, 2, "frame_entries counts the entries written, not the rows that own them");
        let in_zip: i64 = conn.query_row("SELECT COUNT(*) FROM files WHERE archive_zip_path IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(in_zip, 0, "copies leave the originals where they are");
        let files_before: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();

        conn.execute("UPDATE zip_archives SET size = 0, modified_at = ''", []).unwrap();
        // The entries that stay out of the catalog are said once, as a warn, for the changed zip.
        let (outcome, warns) = crate::calibration::geometry::test_support::count_warns(
            crate::zip_store::scan::OPERATION_ENTRIES_SKIPPED,
            || crate::zip_store::scan::scan_zip(&conn, Path::new(&zips[0].0), 1, false).unwrap(),
        );
        assert_eq!(warns, 1, "one warn per zip, not one per entry");
        assert!(!outcome.skipped_unchanged);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert_eq!(outcome.files_inserted, 0, "the copies do not become foreign rows");
        let files_after: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(files_after, files_before);
        let after = zip_rows(&conn);
        assert_eq!(after[0].3, 2, "the rescan keeps the entry count");
        assert_eq!(after[0].4, Some(op_id));
    }

    #[test]
    fn recording_a_rebuilt_zip_clears_the_entries_an_earlier_unpack_released_there() {
        let (conn, _arch, _scan, op_id) = run_full_fixture();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();
        let zip = zip_rows(&conn)[0].0.clone();
        let zip_id: i64 = conn.query_row("SELECT id FROM zip_archives WHERE path = ?1", [&zip], |r| r.get(0)).unwrap();
        conn.execute("INSERT INTO zip_released_entries (zip_id, entry_path) VALUES (?1, 'old/L_000.fits')", [zip_id]).unwrap();

        record_built_zip(&conn, op_id, &zip, 2).unwrap();
        let left: i64 = conn.query_row("SELECT COUNT(*) FROM zip_released_entries WHERE zip_id = ?1", [zip_id], |r| r.get(0)).unwrap();
        assert_eq!(left, 0, "names released from the old zip at this path do not apply to the new one");
        assert_eq!(zip_rows(&conn)[0].3, 2);
    }

    #[test]
    fn the_executor_refuses_to_truncate_a_zip_that_holds_cataloged_frames() {
        let (conn, _arch, _scan, op_id) = run_full_fixture();
        let target: String = conn
            .query_row("SELECT target_zip_path FROM archive_operation_files WHERE operation_id = ?1 LIMIT 1", [op_id], |r| r.get(0))
            .unwrap();
        std::fs::write(&target, b"zip bytes that must survive").unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, archive_zip_path, archive_path_in_zip)
             VALUES (900, ?1, 'X.fits', 1, '2025-01-01T00:00:00Z', 'FITS', ?2, 'X.fits')",
            params![format!("{target}!/X.fits"), target],
        ).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        // The standalone path rolls the failed operation back; that must not remove the zip either.
        let err = run_operation_standalone(&conn, op_id, &cancel, &NullEmitter).unwrap_err();
        assert!(format!("{err:#}").contains("cataloged"), "{err:#}");
        assert_eq!(std::fs::read(&target).unwrap(), b"zip bytes that must survive");
    }

    #[test]
    fn overwrite_replaces_an_old_zip_through_a_partial_and_records_the_rename() {
        let (conn, _arch, _scan, op_id) = run_full_fixture();
        let target: String = conn
            .query_row("SELECT target_zip_path FROM archive_operation_files WHERE operation_id = ?1 LIMIT 1", [op_id], |r| r.get(0))
            .unwrap();
        std::fs::write(&target, b"old zip, no cataloged frames in it").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();

        let zr = zip::ZipArchive::new(std::fs::File::open(&target).unwrap()).unwrap();
        assert_eq!(zr.len(), 2, "the new zip replaced the old bytes");
        let leftovers: Vec<_> = std::fs::read_dir(Path::new(&target).parent().unwrap()).unwrap().flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".partial")).collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let published: i64 = conn.query_row(
            "SELECT COUNT(*) FROM archive_operation_steps WHERE operation_id = ?1 AND stage = 'zip_publish' AND status = 'done'",
            [op_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(published, 1, "one publish step per zip, recorded after the rename");
    }

    #[test]
    fn a_refused_zip_records_no_publish_step() {
        let (conn, _arch, _scan, op_id) = run_full_fixture();
        let target: String = conn
            .query_row("SELECT target_zip_path FROM archive_operation_files WHERE operation_id = ?1 LIMIT 1", [op_id], |r| r.get(0))
            .unwrap();
        std::fs::write(&target, b"zip bytes that must survive").unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, archive_zip_path, archive_path_in_zip)
             VALUES (900, ?1, 'X.fits', 1, '2025-01-01T00:00:00Z', 'FITS', ?2, 'X.fits')",
            params![format!("{target}!/X.fits"), target],
        ).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let _ = run_operation_standalone(&conn, op_id, &cancel, &NullEmitter);
        let steps: i64 = conn.query_row(
            "SELECT COUNT(*) FROM archive_operation_steps WHERE operation_id = ?1 AND stage IN ('zip_publish', 'zip_add')",
            [op_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(steps, 0);
    }

    #[test]
    fn a_resumed_build_clears_leftover_partials_with_this_process_id() {
        let (conn, arch, _scan, op_id) = run_full_fixture();
        let files = adb::list_operation_files(&conn, op_id).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        staging::ensure_staging_dir(arch.path(), op_id).unwrap();
        test_phases::copy_only(&conn, op_id, &files, arch.path(), &cancel).unwrap();
        let target = PathBuf::from(&files[0].target_zip_path);
        let leftovers = crate::archive::zip_writer::tests::leave_own_pid_partials_ahead(&target);

        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();
        assert!(target.exists());
        assert!(leftovers.iter().all(|p| !p.exists()), "the resume removes the crashed run's partials");
    }
}
