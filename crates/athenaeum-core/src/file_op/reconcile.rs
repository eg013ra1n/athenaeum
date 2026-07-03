//! Auto-reconcile abandoned cross-volume Move commits.
//!
//! `executor::run_cross_volume_commit_step` hot-syncs the catalog to point at
//! the destination, then tries to remove the leftover source file. If that
//! removal fails (permission flicker, AV lock, momentary disk hiccup, the
//! process getting killed) the step is marked `Failed` and the file's
//! disposition stays `Failed` — but the destination is already verified and
//! the catalog already points at it. The file-op resume/rollback commands
//! exist on the backend to fix this, but they are not wired to any frontend
//! UI, so nothing ever retries on its own.
//!
//! This module finds exactly that situation and finishes the one thing that
//! didn't complete (`fs::remove_file(source)`) when — and only when — it's
//! provably safe to do so. It never undoes work and never touches a
//! candidate it isn't sure about; anything uncertain is left alone and
//! reported as "skipped" with a reason.
//!
//! Safe-to-heal means, for a given failed CommitMove step + its file row:
//!
//!   1. the destination file still exists on disk;
//!   2. a `Verify` step for the same file reached `Done` with an
//!      `actual_hash` matching the row's `expected_hash` (that's what Verify
//!      already compared against), AND re-hashing the destination right now
//!      still matches `expected_hash` (nothing has touched dest since);
//!   3. the catalog does **not** have a `files` row still pointing at the
//!      source path — if it does, something other than "leftover source
//!      copy" happened (e.g. the hot-sync itself never completed), and that
//!      is not a case this module handles.
//!
//! `reconcile_abandoned_commit_moves` is idempotent: once a candidate is
//! healed its step is `Done`, so it will not be examined again. Skipped
//! candidates stay `Failed` and are re-examined (and typically re-skipped)
//! on every call — that's intentional, since a transient condition (e.g. the
//! destination reappearing) could resolve itself.

use crate::duplicates::compute_xxhash;
use crate::file_op::db::{self as fdb, FailedCommitMoveCandidate};
use crate::file_op::models::{FileDisposition, FileOpStatus, StepStatus};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::Path;

/// One candidate examined but not healed, with the reason why.
#[derive(Debug, Clone)]
pub struct ReconcileOutcome {
    pub operation_id: i64,
    pub source_path: String,
    pub reason: String,
}

/// Result of one `reconcile_abandoned_commit_moves` pass.
#[derive(Debug, Clone, Default)]
pub struct ReconcileSummary {
    /// Total failed-CommitMove candidates examined this pass.
    pub examined: usize,
    /// Count of candidates that were healed.
    pub healed: usize,
    /// Operation ids that had at least one candidate healed.
    pub healed_operation_ids: Vec<i64>,
    /// Candidates that were examined but not healed, with reasons.
    pub skipped: Vec<ReconcileOutcome>,
}

impl ReconcileSummary {
    /// Distinct operation ids touched this pass (healed or skipped). Used to
    /// populate the `file-op-reconciled` event payload.
    pub fn touched_operation_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .healed_operation_ids
            .iter()
            .copied()
            .chain(self.skipped.iter().map(|o| o.operation_id))
            .collect::<HashSet<i64>>()
            .into_iter()
            .collect();
        ids.sort_unstable();
        ids
    }
}

/// Outcome of attempting to heal a single candidate.
enum HealOutcome {
    Healed,
    Skipped(String),
}

/// Scan for abandoned cross-volume Move commits and heal the ones that are
/// provably safe (see module docs). Never aborts on a bad row: any
/// per-candidate error is logged to stderr and treated as a skip so the rest
/// of the batch still runs.
pub fn reconcile_abandoned_commit_moves(conn: &Connection) -> Result<ReconcileSummary> {
    let candidates = fdb::list_failed_commit_move_candidates(conn)?;
    let mut summary = ReconcileSummary {
        examined: candidates.len(),
        ..Default::default()
    };
    let mut healed_operations: HashSet<i64> = HashSet::new();

    for c in &candidates {
        let outcome = match heal_candidate(conn, c) {
            Ok(HealOutcome::Healed) => {
                summary.healed += 1;
                healed_operations.insert(c.operation_id);
                None
            }
            Ok(HealOutcome::Skipped(reason)) => Some(reason),
            Err(e) => Some(format!("reconcile error: {:#}", e)),
        };
        if let Some(reason) = outcome {
            tracing::warn!(
                operation_id = c.operation_id,
                src = %c.source_path,
                reason = %reason,
                "file_op reconcile: skipping candidate"
            );
            summary.skipped.push(ReconcileOutcome {
                operation_id: c.operation_id,
                source_path: c.source_path.clone(),
                reason,
            });
        }
    }

    summary.healed_operation_ids = healed_operations.iter().copied().collect();
    summary.healed_operation_ids.sort_unstable();

    // An operation may now have every file row Done — flip it to Completed.
    // Failures here are logged, not propagated; the file row itself is
    // already healed regardless of whether the parent operation's status
    // catches up.
    for op_id in &healed_operations {
        match fdb::all_operation_files_done(conn, *op_id) {
            Ok(true) => {
                if let Err(e) = fdb::update_operation_status(conn, *op_id, FileOpStatus::Completed, None) {
                    tracing::warn!(
                        operation_id = op_id,
                        error = %e,
                        "file_op reconcile: failed to mark operation completed"
                    );
                }
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(
                operation_id = op_id,
                error = %e,
                "file_op reconcile: checking operation completion failed"
            ),
        }
    }

    if summary.examined > 0 {
        tracing::info!(
            examined = summary.examined,
            healed = summary.healed,
            skipped = summary.skipped.len(),
            "file_op reconcile pass complete"
        );
    }

    Ok(summary)
}

/// Apply the three safety checks to one candidate and, if they all pass,
/// remove the leftover source and mark the step/file/operation done.
fn heal_candidate(conn: &Connection, c: &FailedCommitMoveCandidate) -> Result<HealOutcome> {
    let dest_path = match &c.dest_path {
        Some(p) => p,
        None => return Ok(HealOutcome::Skipped("plan row is missing dest_path".to_string())),
    };
    let expected = match &c.expected_hash {
        Some(h) => h,
        None => return Ok(HealOutcome::Skipped("plan row is missing expected_hash".to_string())),
    };
    let dest = Path::new(dest_path);

    // 1. Destination must still exist.
    if !dest.exists() {
        return Ok(HealOutcome::Skipped(format!(
            "destination missing: {}",
            dest.display()
        )));
    }

    // 2a. A Verify step must have reached Done with a hash matching what the
    //     plan expected — i.e. the file was actually confirmed good once.
    let verify_hash = fdb::verify_step_done_hash(conn, c.operation_id, c.operation_file_id)?;
    match verify_hash {
        Some(h) if &h == expected => {}
        _ => {
            return Ok(HealOutcome::Skipped(
                "unverified: no Done Verify step matching expected_hash".to_string(),
            ));
        }
    }

    // 2b. Re-hash the destination now — nothing should have touched it since
    //     Verify ran.
    let actual = match compute_xxhash(dest) {
        Ok(h) => h,
        Err(e) => {
            return Ok(HealOutcome::Skipped(format!(
                "hashing destination {} failed: {}",
                dest.display(),
                e
            )));
        }
    };
    if &actual != expected {
        return Ok(HealOutcome::Skipped(format!(
            "hash mismatch on {}: expected {}, got {}",
            dest.display(),
            expected,
            actual
        )));
    }

    // 3. The catalog must not still point at the source — that would mean
    //    the hot-sync itself never landed, which is a different failure mode.
    if fdb::catalog_row_exists_at_path(conn, &c.source_path)? {
        return Ok(HealOutcome::Skipped(
            "catalog still references source path".to_string(),
        ));
    }

    // All checks passed — finish the interrupted step. A source that is
    // already gone still counts as healed (nothing left to remove).
    let source = Path::new(&c.source_path);
    if source.exists() {
        if let Err(e) = std::fs::remove_file(source) {
            return Ok(HealOutcome::Skipped(format!(
                "removing leftover source {} failed: {}",
                source.display(),
                e
            )));
        }
    }

    fdb::update_step(conn, c.step_id, StepStatus::Done, None, None)?;
    fdb::update_file_disposition(conn, c.operation_file_id, FileDisposition::Done)?;

    Ok(HealOutcome::Healed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::file_op::db as fdb;
    use crate::file_op::models::{FileOpKind, FileOpStage, MoveStrategy};
    use std::fs;
    use tempfile::TempDir;

    /// Path to the real FITS fixture used by the plate-solve/rustafits
    /// submodule tests — real bytes so `compute_xxhash` exercises the actual
    /// sampling hash path instead of trivially hashing an empty/tiny file.
    const MONO_FITS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../rustafits/tests/mono.fits");

    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    /// Build a failed-cross-volume-move fixture: a `move`-kind operation with
    /// one `file_operation_files` row (strategy `copy_verify_delete`), steps
    /// Copy=Done, Verify=Done (actual_hash set), CommitMove=Failed, and a
    /// catalog `files` row pointing at `dest`. Returns
    /// `(operation_id, operation_file_id, step_id, source, dest)`.
    fn fabricate_failed_cross_volume_move(
        conn: &Connection,
        dir: &TempDir,
        catalog_at_dest: bool,
    ) -> (i64, i64, i64, std::path::PathBuf, std::path::PathBuf) {
        let source = dir.path().join("source.fits");
        let dest = dir.path().join("dest.fits");
        fs::copy(MONO_FITS, &source).unwrap();
        fs::copy(MONO_FITS, &dest).unwrap();
        let hash = compute_xxhash(&dest).unwrap();

        let op_id = fdb::insert_operation(conn, FileOpKind::Move, None, None).unwrap();
        fdb::update_operation_status(conn, op_id, FileOpStatus::Failed, Some("simulated")).unwrap();

        let file_id = fdb::insert_operation_file(
            conn,
            op_id,
            &source.to_string_lossy(),
            Some(&dest.to_string_lossy()),
            MoveStrategy::CopyVerifyDelete,
            None,
            Some(&hash),
            fs::metadata(&dest).unwrap().len() as i64,
        )
        .unwrap();

        let copy_step = fdb::insert_step(conn, op_id, Some(file_id), FileOpStage::Copy).unwrap();
        fdb::update_step(conn, copy_step, StepStatus::Done, None, None).unwrap();
        let verify_step = fdb::insert_step(conn, op_id, Some(file_id), FileOpStage::Verify).unwrap();
        fdb::update_step(conn, verify_step, StepStatus::Done, Some(&hash), None).unwrap();
        let commit_step = fdb::insert_step(conn, op_id, Some(file_id), FileOpStage::CommitMove).unwrap();
        fdb::update_step(
            conn,
            commit_step,
            StepStatus::Failed,
            None,
            Some("removing source after copy succeeded: simulated I/O error"),
        )
        .unwrap();
        fdb::update_file_disposition(conn, file_id, FileDisposition::Failed).unwrap();

        let catalog_path = if catalog_at_dest { &dest } else { &source };
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES (?1, ?2, 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
            [
                &catalog_path.to_string_lossy().to_string(),
                &catalog_path.file_name().unwrap().to_string_lossy().to_string(),
            ],
        )
        .unwrap();

        (op_id, file_id, commit_step, source, dest)
    }

    #[test]
    fn heals_when_dest_verified_and_catalog_points_at_dest() {
        let dir = TempDir::new().unwrap();
        let conn = open_test_db();
        let (op_id, _file_id, step_id, source, dest) =
            fabricate_failed_cross_volume_move(&conn, &dir, true);

        let summary = reconcile_abandoned_commit_moves(&conn).unwrap();

        assert_eq!(summary.examined, 1);
        assert_eq!(summary.healed, 1);
        assert!(summary.skipped.is_empty());
        assert!(!source.exists(), "leftover source should be removed");
        assert!(dest.exists(), "destination must be untouched");

        let step = fdb::list_steps(&conn, op_id)
            .unwrap()
            .into_iter()
            .find(|s| s.id == step_id)
            .unwrap();
        assert_eq!(step.status, "done");

        let files = fdb::list_operation_files(&conn, op_id).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].disposition, "done");

        let op = fdb::get_operation(&conn, op_id).unwrap();
        assert_eq!(op.status, "completed");
    }

    #[test]
    fn hash_mismatch_is_skipped_and_not_healed() {
        let dir = TempDir::new().unwrap();
        let conn = open_test_db();
        let (op_id, _file_id, step_id, source, dest) =
            fabricate_failed_cross_volume_move(&conn, &dir, true);

        // Corrupt the destination after the fixture's Verify step recorded a
        // hash for the original bytes — a fresh re-hash must now disagree.
        let mut bytes = fs::read(&dest).unwrap();
        bytes.extend_from_slice(b"corruption");
        fs::write(&dest, &bytes).unwrap();

        let summary = reconcile_abandoned_commit_moves(&conn).unwrap();

        assert_eq!(summary.examined, 1);
        assert_eq!(summary.healed, 0);
        assert_eq!(summary.skipped.len(), 1);
        assert!(
            summary.skipped[0].reason.to_lowercase().contains("hash"),
            "reason should mention the hash mismatch: {}",
            summary.skipped[0].reason
        );
        assert!(source.exists(), "source must NOT be removed on a skip");

        let step = fdb::list_steps(&conn, op_id)
            .unwrap()
            .into_iter()
            .find(|s| s.id == step_id)
            .unwrap();
        assert_eq!(step.status, "failed");

        let op = fdb::get_operation(&conn, op_id).unwrap();
        assert_eq!(op.status, "failed", "operation status must be left untouched");
    }

    #[test]
    fn catalog_still_at_source_is_skipped() {
        let dir = TempDir::new().unwrap();
        let conn = open_test_db();
        let (_op_id, _file_id, _step_id, source, _dest) =
            fabricate_failed_cross_volume_move(&conn, &dir, false);

        let summary = reconcile_abandoned_commit_moves(&conn).unwrap();

        assert_eq!(summary.healed, 0);
        assert_eq!(summary.skipped.len(), 1);
        assert!(
            summary.skipped[0].reason.to_lowercase().contains("catalog"),
            "reason should mention the catalog: {}",
            summary.skipped[0].reason
        );
        assert!(source.exists(), "source must NOT be removed on a skip");
    }

    #[test]
    fn heals_even_when_source_already_gone() {
        let dir = TempDir::new().unwrap();
        let conn = open_test_db();
        let (op_id, _file_id, step_id, source, _dest) =
            fabricate_failed_cross_volume_move(&conn, &dir, true);

        fs::remove_file(&source).unwrap();

        let summary = reconcile_abandoned_commit_moves(&conn).unwrap();

        assert_eq!(summary.healed, 1);
        assert!(summary.skipped.is_empty());

        let step = fdb::list_steps(&conn, op_id)
            .unwrap()
            .into_iter()
            .find(|s| s.id == step_id)
            .unwrap();
        assert_eq!(step.status, "done");

        let op = fdb::get_operation(&conn, op_id).unwrap();
        assert_eq!(op.status, "completed");
    }

    #[test]
    fn second_pass_after_heal_finds_nothing() {
        let dir = TempDir::new().unwrap();
        let conn = open_test_db();
        fabricate_failed_cross_volume_move(&conn, &dir, true);

        let first = reconcile_abandoned_commit_moves(&conn).unwrap();
        assert_eq!(first.healed, 1);

        let second = reconcile_abandoned_commit_moves(&conn).unwrap();
        assert_eq!(second.examined, 0);
        assert_eq!(second.healed, 0);
        assert!(second.skipped.is_empty());
    }

    #[test]
    fn multi_file_op_completes_only_when_every_row_is_done() {
        let dir = TempDir::new().unwrap();
        let conn = open_test_db();
        let (op_id, _file_id, _step_id, _source, _dest) =
            fabricate_failed_cross_volume_move(&conn, &dir, true);

        // Second row on the SAME operation, already fully done.
        let other_source = dir.path().join("other_source.fits");
        let other_dest = dir.path().join("other_dest.fits");
        fs::copy(MONO_FITS, &other_dest).unwrap();
        let other_file_id = fdb::insert_operation_file(
            &conn,
            op_id,
            &other_source.to_string_lossy(),
            Some(&other_dest.to_string_lossy()),
            MoveStrategy::CopyVerifyDelete,
            None,
            None,
            0,
        )
        .unwrap();
        fdb::update_file_disposition(&conn, other_file_id, FileDisposition::Done).unwrap();

        let summary = reconcile_abandoned_commit_moves(&conn).unwrap();
        assert_eq!(summary.healed, 1);

        let op = fdb::get_operation(&conn, op_id).unwrap();
        assert_eq!(
            op.status, "completed",
            "operation should complete once every row is done"
        );
    }

    #[test]
    fn multi_file_op_not_completed_while_a_sibling_row_is_still_failed() {
        let dir = TempDir::new().unwrap();
        let conn = open_test_db();
        let (op_id, _file_id, _step_id, _source, _dest) =
            fabricate_failed_cross_volume_move(&conn, &dir, true);

        // Second row on the SAME operation, failed at Copy (never reached
        // CommitMove) — reconcile has nothing to heal for it, and it must
        // hold the operation open even after the first row heals.
        let other_source = dir.path().join("other_source.fits");
        let other_file_id = fdb::insert_operation_file(
            &conn,
            op_id,
            &other_source.to_string_lossy(),
            None,
            MoveStrategy::CopyVerifyDelete,
            None,
            None,
            0,
        )
        .unwrap();
        let other_copy_step =
            fdb::insert_step(&conn, op_id, Some(other_file_id), FileOpStage::Copy).unwrap();
        fdb::update_step(
            &conn,
            other_copy_step,
            StepStatus::Failed,
            None,
            Some("simulated copy failure"),
        )
        .unwrap();
        fdb::update_file_disposition(&conn, other_file_id, FileDisposition::Failed).unwrap();

        let summary = reconcile_abandoned_commit_moves(&conn).unwrap();
        assert_eq!(summary.healed, 1);

        let op = fdb::get_operation(&conn, op_id).unwrap();
        assert_eq!(
            op.status, "failed",
            "operation status must not flip to completed while a sibling row is still failed"
        );
    }
}
