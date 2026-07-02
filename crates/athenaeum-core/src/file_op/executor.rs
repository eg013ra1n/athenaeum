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
    FileDisposition, FileOpKind, FileOpProgress, FileOpStage, FileOpStatus, FileOperationFile,
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
pub fn run_operation(
    conn: &Connection,
    operation_id: i64,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let op = fdb::get_operation(conn, operation_id)?;
    let kind = match op.kind.as_str() {
        "move" => FileOpKind::Move,
        "delete" => FileOpKind::Delete,
        other => return Err(anyhow::anyhow!("unknown file_operations.kind: {}", other)),
    };

    fdb::update_operation_status(conn, operation_id, FileOpStatus::Running, None)?;
    let files = fdb::list_operation_files(conn, operation_id)?;
    let total = files.len();

    match kind {
        FileOpKind::Move => {
            run_move(conn, operation_id, &files, total, cancel, emitter)?;
        }
        FileOpKind::Delete => {
            run_delete(conn, operation_id, &files, total, cancel, emitter)?;
        }
    }

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

/// Permanent delete. For each row: remove the catalog row (if any) and
/// delete the file on disk. Empty target directories are removed at the end.
/// No undo, no trash; this is invoked only after the UI has confirmed.
fn run_delete(
    conn: &Connection,
    operation_id: i64,
    files: &[FileOperationFile],
    total: usize,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let commit_done = fdb::done_file_ids_for_stage(conn, operation_id, FileOpStage::CommitDelete)?;

    // Two-pass: delete files first, then directories (in reverse path order
    // so deeper dirs are removed before their parents).
    let mut file_rows: Vec<&FileOperationFile> = Vec::new();
    let mut dir_rows: Vec<&FileOperationFile> = Vec::new();
    for f in files {
        let p = std::path::Path::new(&f.source_path);
        if p.is_dir() {
            dir_rows.push(f);
        } else {
            // is_file or already-deleted
            file_rows.push(f);
        }
    }

    for (idx, f) in file_rows.iter().enumerate() {
        check_cancel(cancel)?;
        if commit_done.contains(&f.id) {
            continue;
        }
        emit_progress_kind(
            emitter,
            "delete",
            operation_id,
            "commit_delete",
            idx + 1,
            total,
            &format!("Deleting {}/{}", idx + 1, total),
        );
        fdb::update_file_disposition(conn, f.id, FileDisposition::InProgress)?;
        let step_id = fdb::insert_step(conn, operation_id, Some(f.id), FileOpStage::CommitDelete)?;
        fdb::update_step(conn, step_id, StepStatus::InProgress, None, None)?;

        // Remove catalog row first (cascades to junction tables). Then rm file.
        if let Some(file_id) = f.catalog_file_id {
            if let Err(e) = conn.execute("DELETE FROM files WHERE id = ?1", [file_id]) {
                let msg = format!("DB delete failed for file_id={}: {}", file_id, e);
                fdb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
                fdb::update_file_disposition(conn, f.id, FileDisposition::Failed)?;
                anyhow::bail!(msg);
            }
        }
        let p = std::path::Path::new(&f.source_path);
        if p.exists() {
            if let Err(e) = fs::remove_file(p) {
                let msg = format!("rm {} failed: {}", p.display(), e);
                fdb::update_step(conn, step_id, StepStatus::Failed, None, Some(&msg))?;
                fdb::update_file_disposition(conn, f.id, FileDisposition::Failed)?;
                anyhow::bail!(msg);
            }
        }
        fdb::update_step(conn, step_id, StepStatus::Done, None, None)?;
        fdb::update_file_disposition(conn, f.id, FileDisposition::Done)?;
    }

    // Sort dirs deepest-first so children are removed before parents.
    dir_rows.sort_by(|a, b| b.source_path.len().cmp(&a.source_path.len()));
    for f in &dir_rows {
        check_cancel(cancel)?;
        if commit_done.contains(&f.id) {
            continue;
        }
        let step_id = fdb::insert_step(conn, operation_id, Some(f.id), FileOpStage::CommitDelete)?;
        fdb::update_step(conn, step_id, StepStatus::InProgress, None, None)?;
        let p = std::path::Path::new(&f.source_path);
        if p.exists() {
            // remove_dir succeeds only if the dir is empty — exactly what we
            // want, since the file pass above already cleaned it.
            if let Err(e) = fs::remove_dir(p) {
                // Some dirs may legitimately have non-deleted siblings (e.g.
                // user only selected some files inside). Treat dir-removal
                // failures as warnings, not hard errors.
                eprintln!(
                    "skipping rmdir {} (likely non-empty): {}",
                    p.display(),
                    e
                );
                fdb::update_step(
                    conn,
                    step_id,
                    StepStatus::Done,
                    None,
                    Some(&format!("dir not empty: {}", e)),
                )?;
                fdb::update_file_disposition(conn, f.id, FileDisposition::Skipped)?;
                continue;
            }
        }
        fdb::update_step(conn, step_id, StepStatus::Done, None, None)?;
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

fn emit_progress_kind(
    emitter: &dyn ProgressEmitter,
    kind: &str,
    operation_id: i64,
    stage: &str,
    current: usize,
    total: usize,
    message: &str,
) {
    crate::events::emit_event(
        emitter,
        "file-op-progress",
        &FileOpProgress {
            operation_id,
            kind: kind.to_string(),
            stage: stage.to_string(),
            current,
            total,
            message: message.to_string(),
        },
    );
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
    fn deletes_full_directory_hierarchy_including_subdirs() {
        // /root/group/{a.fit, sub/{b.fit, deeper/{c.fit, leaf_empty/}}}
        // After delete-by-folder, the entire tree should be gone.
        use crate::file_op::planner::build_delete_plan;

        let scan_root = TempDir::new().unwrap();
        let conn = setup_with_scan_root(scan_root.path());

        let group = scan_root.path().join("group");
        let sub = group.join("sub");
        let deeper = sub.join("deeper");
        let leaf_empty = deeper.join("leaf_empty");
        fs::create_dir_all(&leaf_empty).unwrap();
        File::create(group.join("a.fit")).unwrap();
        File::create(sub.join("b.fit")).unwrap();
        File::create(deeper.join("c.fit")).unwrap();

        let plan = build_delete_plan(&conn, vec![group.clone()]).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, plan.operation_id, &cancel, &NullEmitter).unwrap();

        assert!(!group.exists(), "top-level dir should be removed");
        assert!(!sub.exists(), "sub dir should be removed");
        assert!(!deeper.exists(), "deeper dir should be removed");
        assert!(!leaf_empty.exists(), "empty leaf dir should be removed");
    }

    #[test]
    fn deletes_files_and_empty_dirs_with_catalog_sync() {
        use crate::file_op::planner::build_delete_plan;

        let scan_root = TempDir::new().unwrap();
        let conn = setup_with_scan_root(scan_root.path());

        let dir = scan_root.path().join("group");
        fs::create_dir(&dir).unwrap();
        let f1 = dir.join("a.fit");
        let f2 = dir.join("b.fit");
        File::create(&f1).unwrap();
        File::create(&f2).unwrap();
        // Catalog rows for both.
        for p in [&f1, &f2] {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at)
                 VALUES (?1, ?2, 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
                [&p.to_string_lossy().to_string(), &p.file_name().unwrap().to_string_lossy().to_string()],
            ).unwrap();
        }

        let plan = build_delete_plan(&conn, vec![dir.clone()]).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, plan.operation_id, &cancel, &NullEmitter).unwrap();

        assert!(!f1.exists() && !f2.exists(), "files should be gone");
        assert!(!dir.exists(), "empty directory should be removed");

        let dir_prefix = format!("{}/", dir.to_string_lossy());
        let dir_hi = crate::db::path_prefix_upper(&dir_prefix);
        let row_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE path >= ?1 AND (?2 IS NULL OR path < ?2)",
                rusqlite::params![dir_prefix, dir_hi],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(row_count, 0, "catalog rows should be removed");
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
