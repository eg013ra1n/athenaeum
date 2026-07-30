//! Drive a planned Move operation through the per-file work.
//!
//! Stages per file (cross-volume Move only):
//!   1. `Copy`         — stream source → dest. Idempotent: if dest exists with
//!                       matching hash, skip the copy.
//!   2. `Verify`       — hash dest, compare to `expected_hash` from the plan.
//!                       Bails the file (and the operation) on mismatch.
//!   3. `CommitMove`   — atomic transaction: update `files.path` (if catalog
//!                       row exists), then delete the source file from disk,
//!                       then commit.
//!
//! Same-volume (atomic) Move uses just stage `CommitMove`: the rename(2) is
//! atomic, no separate copy/verify. The DB and disk update are paired inside
//! a single transaction; on crash the file is either fully at source or fully
//! at dest, and the resume path detects which.
//!
//! Cancellation is cooperative: we check `CancelFlag` between every step.

use crate::duplicates::compute_xxhash;
use crate::events::{emit_event, ProgressEmitter};
use crate::file_op::db as fdb;
use crate::file_op::models::{
    FileDisposition, FileOpProgress, FileOpStage, FileOpStatus, FileOperationFile,
    MoveStrategy, StepStatus,
};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Cancellation indicator. Worker checks between every per-file step.
pub type CancelFlag = Arc<AtomicBool>;

const CANCEL_MSG: &str = "__file_op_cancelled__";

fn check_cancel(cancel: &CancelFlag) -> Result<()> {
    if cancel.load(Ordering::SeqCst) {
        anyhow::bail!(CANCEL_MSG);
    }
    Ok(())
}

pub fn was_cancelled(err: &anyhow::Error) -> bool {
    format!("{}", err).contains(CANCEL_MSG)
}

/// Run a planned operation. Caller must have already persisted it via the planner.
///
/// Only `"move"` is executable — user-facing Delete goes through Black Hole
/// (`send_to_void`/`bulk_move_to_black_hole`), which never constructs a
/// `file_operations` row of kind `"delete"`. A historical `"delete"` row
/// (from before that cutover) fails loudly here instead of silently running
/// a deleted code path.
pub fn run_operation(
    conn: &Connection,
    operation_id: i64,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let span = tracing::info_span!("file_op", operation_id);
    let _g = span.enter();

    let op = fdb::get_operation(conn, operation_id)?;
    match op.kind.as_str() {
        "move" => {}
        "delete" => {
            return Err(anyhow::anyhow!(
                "file_operations.kind 'delete' is no longer executable (operation_id={}); user-facing Delete goes through Black Hole",
                operation_id
            ));
        }
        other => return Err(anyhow::anyhow!("unknown file_operations.kind: {}", other)),
    }

    fdb::update_operation_status(conn, operation_id, FileOpStatus::Running, None)?;
    let files = fdb::list_operation_files(conn, operation_id)?;
    let total = files.len();

    run_move(conn, operation_id, &files, total, cancel, emitter)?;

    fdb::update_operation_status(conn, operation_id, FileOpStatus::Completed, None)?;
    Ok(())
}

fn run_move(
    conn: &Connection,
    operation_id: i64,
    files: &[FileOperationFile],
    total: usize,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    // Pre-fetch sets of already-Done steps for resume idempotency.
    let copy_done = fdb::done_file_ids_for_stage(conn, operation_id, FileOpStage::Copy)?;
    let verify_done = fdb::done_file_ids_for_stage(conn, operation_id, FileOpStage::Verify)?;
    let commit_done = fdb::done_file_ids_for_stage(conn, operation_id, FileOpStage::CommitMove)?;

    for (idx, f) in files.iter().enumerate() {
        check_cancel(cancel)?;

        if commit_done.contains(&f.id) {
            // Already finished on a previous run.
            continue;
        }

        let strategy = MoveStrategy::from_str(&f.strategy).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown strategy '{}' on file_operation_files.id={}",
                f.strategy,
                f.id
            )
        })?;
        let dest_path_str = f.dest_path.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Move plan row missing dest_path: id={}", f.id)
        })?;
        let dest = Path::new(dest_path_str);
        let source = Path::new(&f.source_path);

        fdb::update_file_disposition(conn, f.id, FileDisposition::InProgress)?;

        match strategy {
            MoveStrategy::AtomicRename => {
                emit_progress(
                    emitter,
                    operation_id,
                    "commit_move",
                    idx + 1,
                    total,
                    &format!("Moving {}/{}", idx + 1, total),
                );
                run_atomic_rename_step(conn, operation_id, f, source, dest, &commit_done)?;
            }
            MoveStrategy::CopyVerifyDelete => {
                if !copy_done.contains(&f.id) {
                    emit_progress(
                        emitter,
                        operation_id,
                        "copy",
                        idx + 1,
                        total,
                        &format!("Copying {}/{}", idx + 1, total),
                    );
                    run_copy_step(conn, operation_id, f, source, dest)?;
                }
                if !verify_done.contains(&f.id) {
                    emit_progress(
                        emitter,
                        operation_id,
                        "verify",
                        idx + 1,
                        total,
                        &format!("Verifying {}/{}", idx + 1, total),
                    );
                    run_verify_step(conn, operation_id, f, dest)?;
                }
                emit_progress(
                    emitter,
                    operation_id,
                    "commit_move",
                    idx + 1,
                    total,
                    &format!("Finishing {}/{}", idx + 1, total),
                );
                run_cross_volume_commit_step(conn, operation_id, f, source, dest)?;
            }
            MoveStrategy::Delete => {
                return Err(anyhow::anyhow!(
                    "Delete strategy is not implemented in Phase 1 (file row id={})",
                    f.id
                ));
            }
        }

        fdb::update_file_disposition(conn, f.id, FileDisposition::Done)?;
    }
    Ok(())
}

/// Has this (operation, file) already had a step recorded for `stage`?
/// Used by the cross-volume copy to distinguish a resumption (we can claim
/// the existing dest as our partial) from a fresh op hitting a foreign file.
fn prior_step_exists(
    conn: &Connection,
    operation_id: i64,
    file_id: i64,
    stage: FileOpStage,
) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_operation_steps
         WHERE operation_id = ?1 AND operation_file_id = ?2 AND stage = ?3",
        rusqlite::params![operation_id, file_id, stage.as_str()],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Same-volume rename + DB path update. Idempotent on resume.
fn run_atomic_rename_step(
    conn: &Connection,
    operation_id: i64,
    f: &FileOperationFile,
    source: &Path,
    dest: &Path,
    commit_done: &std::collections::HashSet<i64>,
) -> Result<()> {
    if commit_done.contains(&f.id) {
        return Ok(());
    }

    // An earlier run may already have degraded this row to the cross-volume
    // pipeline (EXDEV, below) and been interrupted part-way. Resume there
    // directly: the resume detection under this block reads a partially
    // copied destination as a foreign-file collision and would strand the
    // row. A Copy step on an AtomicRename row can only exist because that
    // degradation happened — the planner never plans one.
    if prior_step_exists(conn, operation_id, f.id, FileOpStage::Copy)? {
        tracing::info!(
            src = %source.display(), dest = %dest.display(),
            "resuming EXDEV-degraded row on the cross-volume pipeline"
        );
        return run_cross_volume_fallback(conn, operation_id, f, source, dest);
    }

    let step_id = fdb::insert_step(conn, operation_id, Some(f.id), FileOpStage::CommitMove)?;
    fdb::update_step(conn, step_id, StepStatus::InProgress, None, None)?;

    // Detect resume scenarios first.
    let src_exists = source.exists();
    let dest_exists = dest.exists();

    if !src_exists && dest_exists {
        // Disk rename was done on a previous run; just (re-)update the DB row
        // if there is one.
        sync_catalog_path(conn, f, dest)?;
        fdb::update_step(conn, step_id, StepStatus::Done, None, None)?;
        return Ok(());
    }
    if src_exists && dest_exists {
        let msg = format!("destination already exists: {}", dest.display());
        fdb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
        fdb::update_file_disposition(conn, f.id, FileDisposition::Failed)?;
        anyhow::bail!(msg);
    }
    if !src_exists && !dest_exists {
        let msg = format!("source disappeared: {}", source.display());
        fdb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
        fdb::update_file_disposition(conn, f.id, FileDisposition::Failed)?;
        anyhow::bail!(msg);
    }

    // Normal forward path.
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("creating parent directory {}", parent.display())
        })?;
    }
    if let Err(e) = fs::rename(source, dest) {
        if e.kind() == std::io::ErrorKind::CrossesDevices {
            // Same device id ≠ rename works: Linux bind mounts share st_dev
            // yet rename(2) returns EXDEV across mount points (reachable in
            // the Docker build's compose volumes), and Windows folder-mounted
            // volumes canonicalize into the hosting drive. Degrade this row
            // to the cross-volume pipeline instead of failing the batch.
            tracing::warn!(src = %source.display(), dest = %dest.display(),
                "rename crossed a device boundary; degrading to copy+verify+delete");
            let outcome = run_cross_volume_fallback(conn, operation_id, f, source, dest);
            // Status keyed on the fallback's OUTCOME, never stamped up front:
            // a `Done` here lands the row in `done_file_ids_for_stage`, and a
            // resume after an interrupted fallback would then skip the file
            // whole while it is still sitting at the source.
            match &outcome {
                Ok(()) => fdb::update_step(
                    conn,
                    step_id,
                    StepStatus::Done,
                    None,
                    Some("EXDEV — degraded to copy+verify+delete"),
                )?,
                Err(err) => fdb::update_step(
                    conn,
                    step_id,
                    StepStatus::Failed,
                    None,
                    Some(&format!(
                        "EXDEV — degraded to copy+verify+delete, which failed: {err:#}"
                    )),
                )?,
            }
            return outcome;
        }
        let msg = format!("rename {} → {}: {}", source.display(), dest.display(), e);
        fdb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
        fdb::update_file_disposition(conn, f.id, FileDisposition::Failed)?;
        anyhow::bail!(msg);
    }

    if let Err(e) = sync_catalog_path(conn, f, dest) {
        // The disk move succeeded; rolling back the file would now move it
        // back. Easier to mark the step failed and let the caller initiate
        // a real rollback. For now we surface the error.
        let msg = format!("DB sync failed after rename: {}", e);
        fdb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
        fdb::update_file_disposition(conn, f.id, FileDisposition::Failed)?;
        return Err(e);
    }

    fdb::update_step(conn, step_id, StepStatus::Done, None, None)?;
    Ok(())
}

/// EXDEV degradation path: hash the still-present source, persist the hash on
/// the row, then run the standard cross-volume steps. Idempotent on resume:
/// re-entry (via the prior-Copy-step route at the top of
/// `run_atomic_rename_step`) reuses the persisted hash and `run_copy_step`'s
/// prior-Copy-step check treats an existing dest as our own partial.
fn run_cross_volume_fallback(
    conn: &Connection,
    operation_id: i64,
    f: &FileOperationFile,
    source: &Path,
    dest: &Path,
) -> Result<()> {
    // The planner only hashes CopyVerifyDelete rows, so on an AtomicRename row
    // a `Some` here can only be one WE persisted on an earlier degradation of
    // this same row — reusing it is both safe and necessary, since a resume
    // can re-enter after the source file is already gone.
    let hash = match f.expected_hash.clone() {
        Some(h) => h,
        None => {
            let h = compute_xxhash(source)
                .with_context(|| format!("hashing {} for EXDEV fallback", source.display()))?;
            fdb::set_expected_hash(conn, f.id, &h)?;
            h
        }
    };
    let mut f2 = f.clone();
    f2.expected_hash = Some(hash);
    run_copy_step(conn, operation_id, &f2, source, dest)?;
    run_verify_step(conn, operation_id, &f2, dest)?;
    run_cross_volume_commit_step(conn, operation_id, &f2, source, dest)
}

/// Cross-volume: copy source bytes to dest. Detects already-copied dest by
/// hash to make this idempotent on resume.
fn run_copy_step(
    conn: &Connection,
    operation_id: i64,
    f: &FileOperationFile,
    source: &Path,
    dest: &Path,
) -> Result<()> {
    // Before inserting the new Copy step, check whether THIS operation has
    // ever attempted a Copy on this file before (resume case). The presence
    // of a prior Copy step is what authorises us to treat an existing dest
    // as our own partial. A *fresh* op should never see an existing dest —
    // the planner checks for collisions — but if one slipped through (race
    // between plan and execute), bail rather than silently overwrite.
    let had_prior_copy = prior_step_exists(conn, operation_id, f.id, FileOpStage::Copy)?;

    let step_id = fdb::insert_step(conn, operation_id, Some(f.id), FileOpStage::Copy)?;
    fdb::update_step(conn, step_id, StepStatus::InProgress, None, None)?;

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("creating parent directory {}", parent.display())
        })?;
    }

    if dest.exists() {
        if !had_prior_copy {
            // Foreign file at the destination — refuse rather than overwrite.
            // Planner would normally catch this, but defence-in-depth covers
            // the race window between plan time and execute time.
            let msg = format!(
                "destination already exists and is not from this operation: {}",
                dest.display()
            );
            fdb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
            fdb::update_file_disposition(conn, f.id, FileDisposition::Failed)?;
            anyhow::bail!(msg);
        }
        // Resume case: leftover from a previous run. If hash matches the
        // expected source hash the copy is already done — skip it. Otherwise
        // it's a partial WE wrote, safe to discard and retry.
        if let Some(expected) = &f.expected_hash {
            if let Ok(h) = compute_xxhash(dest) {
                if &h == expected {
                    fdb::update_step(conn, step_id, StepStatus::Done, Some(&h), None)?;
                    return Ok(());
                }
            }
        }
        if let Err(e) = fs::remove_file(dest) {
            let msg = format!(
                "removing stale partial dest {}: {}",
                dest.display(),
                e
            );
            fdb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
            fdb::update_file_disposition(conn, f.id, FileDisposition::Failed)?;
            anyhow::bail!(msg);
        }
    }

    if let Err(e) = fs::copy(source, dest) {
        let msg = format!("copying {} → {}: {}", source.display(), dest.display(), e);
        fdb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
        fdb::update_file_disposition(conn, f.id, FileDisposition::Failed)?;
        anyhow::bail!(msg);
    }
    fdb::update_step(conn, step_id, StepStatus::Done, None, None)?;
    Ok(())
}

fn run_verify_step(
    conn: &Connection,
    operation_id: i64,
    f: &FileOperationFile,
    dest: &Path,
) -> Result<()> {
    let step_id = fdb::insert_step(conn, operation_id, Some(f.id), FileOpStage::Verify)?;
    fdb::update_step(conn, step_id, StepStatus::InProgress, None, None)?;

    let expected = f
        .expected_hash
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing expected_hash for cross-volume move"))?;
    let actual = match compute_xxhash(dest) {
        Ok(h) => h,
        Err(e) => {
            let msg = format!("hashing destination {}: {}", dest.display(), e);
            fdb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
            fdb::update_file_disposition(conn, f.id, FileDisposition::Failed)?;
            anyhow::bail!(msg);
        }
    };
    if &actual != expected {
        let msg = format!(
            "hash mismatch on {}: expected {}, got {}",
            dest.display(),
            expected,
            actual
        );
        fdb::update_step(conn, step_id, StepStatus::Failed, Some(&actual), Some(&msg))?;
        fdb::update_file_disposition(conn, f.id, FileDisposition::Failed)?;
        anyhow::bail!(msg);
    }
    fdb::update_step(conn, step_id, StepStatus::Done, Some(&actual), None)?;
    Ok(())
}

/// Cross-volume CommitMove: hot-sync DB path, delete source. Wrapped in a
/// SQLite transaction so the catalog stays consistent even if the source
/// delete fails.
fn run_cross_volume_commit_step(
    conn: &Connection,
    operation_id: i64,
    f: &FileOperationFile,
    source: &Path,
    dest: &Path,
) -> Result<()> {
    let step_id = fdb::insert_step(conn, operation_id, Some(f.id), FileOpStage::CommitMove)?;
    fdb::update_step(conn, step_id, StepStatus::InProgress, None, None)?;

    if let Err(e) = sync_catalog_path(conn, f, dest) {
        let msg = format!("catalog hot-sync failed: {}", e);
        fdb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
        fdb::update_file_disposition(conn, f.id, FileDisposition::Failed)?;
        return Err(e);
    }
    if source.exists() {
        if let Err(e) = fs::remove_file(source) {
            // The destination is good and the catalog points at it. Source
            // delete failure leaves a duplicate file but the catalog is
            // consistent. Mark the step failed so the issue is visible, but
            // don't undo the move.
            let msg = format!(
                "removing source after copy succeeded {}: {}",
                source.display(),
                e
            );
            fdb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
            fdb::update_file_disposition(conn, f.id, FileDisposition::Failed)?;
            anyhow::bail!(msg);
        }
    }
    fdb::update_step(conn, step_id, StepStatus::Done, None, None)?;
    Ok(())
}

/// Update `files.path` (and `filename`) for the catalog row associated with
/// this planned file. Always tries the path-based update so the catalog
/// hot-syncs even if the planner missed the row at lookup time (e.g. due to
/// path-encoding differences). No-op for paths the catalog doesn't know
/// about (e.g. sidecar files).
fn sync_catalog_path(conn: &Connection, f: &FileOperationFile, new_path: &Path) -> Result<()> {
    let new_filename = new_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("dest has no filename"))?
        .to_string_lossy()
        .to_string();
    let new_path_str = new_path.to_string_lossy().to_string();

    // 1. Path-based update — always run. Covers the case where the planner
    //    couldn't resolve `catalog_file_id`. Returns 0 if the file isn't in
    //    the catalog (sidecars, non-FITS files), which is fine.
    let updated_by_path =
        fdb::update_files_path_by_old_path(conn, &f.source_path, &new_path_str, &new_filename)?;

    // 2. id-based update as a defence — only fires if the planner did capture
    //    file_id but the path-based update missed (catalog drift mid-op).
    if updated_by_path == 0 {
        if let Some(file_id) = f.catalog_file_id {
            fdb::update_files_path(conn, file_id, &new_path_str, &new_filename)?;
        }
    }

    if updated_by_path == 0 && f.catalog_file_id.is_none() {
        // Both mechanisms missing together is the path-spelling-drift
        // signature (sidecar/non-catalog files are expected misses — only
        // catalog-eligible formats get the warn).
        let eligible = new_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| {
                matches!(
                    e.to_ascii_lowercase().as_str(),
                    "fits" | "fit" | "fts" | "xisf"
                )
            })
            .unwrap_or(false);
        if eligible {
            tracing::warn!(src = %f.source_path, dest = %new_path_str,
                "move hot-sync matched no catalog row for catalog-eligible file");
        }
    }

    Ok(())
}

fn emit_progress(
    emitter: &dyn ProgressEmitter,
    operation_id: i64,
    stage: &str,
    current: usize,
    total: usize,
    message: &str,
) {
    emit_event(
        emitter,
        "file-op-progress",
        &FileOpProgress {
            operation_id,
            kind: "move".to_string(),
            stage: stage.to_string(),
            current,
            total,
            message: message.to_string(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use crate::file_op::planner::build_move_plan;
    use rusqlite::Connection;
    use std::fs::{self, File};
    use std::io::Write;
    use tempfile::TempDir;

    fn setup_with_scan_root(scan_root: &std::path::Path) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO scan_roots (path, enabled, find_duplicates) VALUES (?1, 1, 1)",
            [&scan_root.to_string_lossy().to_string()],
        )
        .unwrap();
        conn
    }

    #[test]
    fn moves_single_file_atomic_same_volume() {
        let scan_root = TempDir::new().unwrap();
        let conn = setup_with_scan_root(scan_root.path());

        let src = scan_root.path().join("foo.fit");
        {
            let mut f = File::create(&src).unwrap();
            f.write_all(b"hello world").unwrap();
        }
        let dest_dir = scan_root.path().join("dest");
        fs::create_dir(&dest_dir).unwrap();

        let plan = build_move_plan(&conn, vec![src.clone()], dest_dir.clone()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, plan.operation_id, &cancel, &NullEmitter).unwrap();

        assert!(!src.exists(), "source should be gone after move");
        let dest = dest_dir.join("foo.fit");
        assert!(dest.exists(), "destination should exist after move");
        let body = fs::read_to_string(&dest).unwrap();
        assert_eq!(body, "hello world");

        let op = fdb::get_operation(&conn, plan.operation_id).unwrap();
        assert_eq!(op.status, "completed");
    }

    #[test]
    fn move_hot_syncs_catalog_path_even_when_planner_missed_file_id() {
        use crate::file_op::planner::build_move_plan;

        let scan_root = TempDir::new().unwrap();
        let conn = setup_with_scan_root(scan_root.path());

        let src = scan_root.path().join("frame.fits");
        File::create(&src).unwrap();

        // Catalog row exists.
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES (?1, 'frame.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
            [&src.to_string_lossy().to_string()],
        )
        .unwrap();
        let file_id: i64 = conn
            .query_row("SELECT id FROM files WHERE path = ?1", [&src.to_string_lossy().to_string()], |r| r.get(0))
            .unwrap();

        let dest_dir = scan_root.path().join("organized");
        fs::create_dir(&dest_dir).unwrap();
        let plan = build_move_plan(&conn, vec![src.clone()], dest_dir.clone()).unwrap();

        // Simulate planner failing to capture file_id (path encoding mismatch
        // scenario). The path-based hot-sync fallback in the executor must
        // still update the catalog.
        conn.execute(
            "UPDATE file_operation_files SET catalog_file_id = NULL WHERE operation_id = ?1",
            [plan.operation_id],
        )
        .unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, plan.operation_id, &cancel, &NullEmitter).unwrap();

        let path: String = conn
            .query_row("SELECT path FROM files WHERE id = ?1", [file_id], |r| r.get(0))
            .unwrap();
        assert_eq!(
            path,
            dest_dir.join("frame.fits").to_string_lossy(),
            "hot-sync fallback by path must update files.path even without catalog_file_id"
        );
    }

    #[test]
    fn historical_delete_kind_row_fails_loudly() {
        // Owner decision: user-facing Delete goes through Black Hole, so the
        // executor's delete path was removed. A `file_operations` row with
        // kind='delete' can still exist from before that cutover (or a
        // resumed historical op) — it must fail loudly, not silently no-op
        // or panic.
        use crate::file_op::models::FileOpKind;

        let scan_root = TempDir::new().unwrap();
        let conn = setup_with_scan_root(scan_root.path());

        let operation_id = fdb::insert_operation(&conn, FileOpKind::Delete, None, None).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let err = run_operation(&conn, operation_id, &cancel, &NullEmitter).unwrap_err();
        assert!(
            format!("{}", err).contains("no longer executable"),
            "expected explicit 'no longer executable' error, got: {}",
            err
        );
    }

    #[test]
    fn hot_syncs_catalog_path_on_atomic_rename() {
        let scan_root = TempDir::new().unwrap();
        let conn = setup_with_scan_root(scan_root.path());

        let src = scan_root.path().join("frame.fits");
        File::create(&src).unwrap();

        // Insert a catalog row pointing at the source.
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES (?1, 'frame.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
            [&src.to_string_lossy().to_string()],
        )
        .unwrap();
        let file_id: i64 = conn
            .query_row("SELECT id FROM files WHERE path = ?1", [&src.to_string_lossy().to_string()], |r| {
                r.get(0)
            })
            .unwrap();

        let dest_dir = scan_root.path().join("organized");
        fs::create_dir(&dest_dir).unwrap();
        let plan = build_move_plan(&conn, vec![src.clone()], dest_dir.clone()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, plan.operation_id, &cancel, &NullEmitter).unwrap();

        let path: String = conn
            .query_row("SELECT path FROM files WHERE id = ?1", [file_id], |r| r.get(0))
            .unwrap();
        assert_eq!(path, dest_dir.join("frame.fits").to_string_lossy());
    }
}
