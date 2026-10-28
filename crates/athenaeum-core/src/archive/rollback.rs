//! Roll back an archive operation by reading its step log.
//!
//! Rollback strategy depends on how far the operation got:
//! - Through `zip_verifying`: source files untouched. Just delete partial zips +
//!   staging dir.
//! - During `deleting_sources` or `finalizing`: some sources already deleted;
//!   restore each deleted source from staging back to its original path,
//!   then delete the zip(s) + staging dir, then unmark catalog rows.
//!
//! The zip model: a zip is built in a `<zip>.<pid>-<n>.partial` beside its target and renamed onto it
//! when complete, and the executor records a `zip_publish` step right after that rename. Rollback
//! removes (a) the operation's stale partials beside its targets and (b) a target zip only when a
//! `zip_publish` step says this operation put it there. It refuses to start while a zip it would
//! delete may hold the only copy of a frame (a moved source that is neither intact on disk nor in staging).
//!
//! Residual, accepted: a failure in a later phase, after the rename, rolls back by deleting the NEW zip;
//! when the build replaced an old zip (Overwrite), that old file was already replaced by the user's own
//! choice and does not come back (a zip with cataloged frames is refused earlier, so only uncataloged
//! content is ever overwritten).

use crate::archive::db as adb;
use crate::archive::models::{
    ArchiveStage, ArchiveStatus, StepStatus,
};
use crate::archive::staging;
use crate::archive::zip_writer::sweep_stale_partials_beside;
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

#[derive(Serialize)]
struct RollbackFinished<'a> {
    operation_id: i64,
    outcome: &'a str,
    kind: &'a str,
}

/// User-initiated rollback (the "Roll back" action on an interrupted
/// operation): the rollback *is* the whole operation, so it owns the terminal
/// `archive-finished` event the progress widget waits for.
///
/// The failure-path callers keep calling [`rollback_operation`] instead,
/// because there a rollback is a sub-step of a failed operation, not that
/// operation's terminal state — the outcome the UI must end on is the archive's
/// (`failed` / `cancelled`), not the rollback's.
///
/// The forward-path callers get the same treatment from
/// `executor::run_operation_standalone` / `resume::resume_operation_standalone`
/// (one shared terminal block in core, both hosts' workers call it); the
/// calibration-archive worker (`api::masters::archive_originals`) still emits
/// its own richer terminal payload. Only the *inner* [`rollback_operation`]
/// stays event-free — there a rollback is a sub-step of a failed operation,
/// and the outcome the UI must end on is the archive's, not the rollback's.
pub fn rollback_operation_standalone(
    conn: &Connection,
    operation_id: i64,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let result = rollback_operation(conn, operation_id, emitter);
    emit_event(emitter, "archive-finished", &RollbackFinished {
        operation_id,
        outcome: if result.is_ok() { "completed" } else { "failed" },
        kind: "rollback",
    });
    result
}

/// Roll back a forward operation. Idempotent: re-running on a partially-rolled-back
/// op is safe.
pub fn rollback_operation(
    conn: &Connection,
    operation_id: i64,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let op = adb::get_operation(conn, operation_id)?;
    // A finished operation has no staging left to restore from: rolling it back would delete its zip
    // and unmark its rows while the sources are gone. Unarchive is the way back.
    if op.status == ArchiveStatus::Completed.as_str() {
        tracing::error!(operation_id, "rollback of a completed operation refused");
        anyhow::bail!("a completed operation cannot be rolled back; use Unarchive");
    }
    // A rolled-back operation's zips are gone or belong to a later operation at the same path: its step
    // log names a zip it no longer owns. (`rolling_back` stays allowed: re-running an interrupted rollback.)
    if op.status == ArchiveStatus::RolledBack.as_str() {
        tracing::error!(operation_id, "rollback of an already rolled-back operation refused");
        anyhow::bail!("this operation is already rolled back");
    }
    let archive_root = PathBuf::from(&op.archive_root_path);
    let files = adb::list_operation_files(conn, operation_id)?;
    // A DeleteSource step in ANY status counts: the executor inserts it InProgress before it removes the
    // source, so a crash (or a failed status write) between the remove and the Done update leaves the
    // source gone with the step still InProgress. Only the staged copy then has the frame.
    let deleted_file_ids = file_ids_with_any_step(conn, operation_id, ArchiveStage::DeleteSource)?;
    let published = file_ids_with_done_step(conn, operation_id, ArchiveStage::ZipPublish)?;
    let published_zips: HashSet<&str> = files
        .iter()
        .filter(|f| published.contains(&f.id))
        .map(|f| f.target_zip_path.as_str())
        .collect();

    // The files this rollback restores: a move file whose DeleteSource step exists in any status, or
    // whose source is gone while a zip of this operation stands.
    let to_restore: Vec<&crate::archive::models::ArchiveOperationFile> = files
        .iter()
        .filter(|f| f.disposition == "move")
        .filter(|f| deleted_file_ids.contains(&f.id) || (!published_zips.is_empty() && !Path::new(&f.source_path).exists()))
        .collect();

    // Before anything is touched: a zip this rollback would delete (or rows it would unmark) must not
    // be the only copy of a frame. Every such file has to be intact at its source (the catalog hash
    // matches) or restorable from staging. Gone from both (staging cleaned after the sources were
    // deleted, the archive drive unplugged, a truncated leftover of an earlier failed restore) means
    // the zip is the only copy: refuse, change nothing, and leave the status as it is.
    let lost: Vec<&str> = to_restore
        .iter()
        .filter(|f| {
            !source_matches(f)
                && !staging::staging_file_path(&archive_root, operation_id, &f.target_path_in_zip).exists()
        })
        .map(|f| f.source_path.as_str())
        .collect();
    if !lost.is_empty() {
        tracing::error!(
            operation_id,
            lost = lost.len(),
            first = lost[0],
            status = %op.status,
            "rollback refused: moved sources are neither intact on disk nor in staging, the zip holds the only copy"
        );
        anyhow::bail!(
            "cannot roll back: {} moved source file(s) (first: {}) are neither intact on disk nor in staging, so the archive \
             zip may hold their only copy; nothing was changed. Reconnect the archive folder or resume the operation instead",
            lost.len(),
            lost[0]
        );
    }

    adb::update_operation_status(conn, operation_id, ArchiveStatus::RollingBack, None)?;

    // 1. Restore any deleted sources from staging.
    //
    // `total` counts only the files this loop will actually restore, and
    // `restored` counts them off as it goes — neither may be derived from the
    // position in `files`. Partial deletion is the normal rollback shape (an
    // interrupted operation deleted a prefix of the batch), and a Done
    // DeleteSource step is *not* the same as "a source was deleted": a
    // copy-disposition calibration gets that step too, with nothing removed
    // from disk (see `executor::delete_sources_phase`).
    let total = to_restore.len();
    let mut restored = 0usize;
    for f in to_restore {
        restored += 1;
        emit_event(emitter, "archive-progress", &RollbackProgress {
            operation_id,
            stage: "restore_source".into(),
            current: restored,
            total,
            message: format!("Restoring source {}/{}", restored, total),
        });

        let staged = staging::staging_file_path(&archive_root, operation_id, &f.target_path_in_zip);
        let target = Path::new(&f.source_path);

        // An existing source is trusted only when it hashes to the catalog's value: a copy that failed or
        // was killed halfway leaves a truncated file at the path, which must not count as restored.
        if !source_matches(f) && staged.exists() {
            restore_from_staging(&staged, target)?;
        }

        // Mark a restore_source step as Done so resume understands progress.
        let sid = adb::insert_step(conn, operation_id, Some(f.id), ArchiveStage::RestoreSource)?;
        adb::update_step(conn, sid, StepStatus::Done, None, None)?;
    }

    // 2. Delete the zips this operation put in place, and its partials.
    //
    // Provenance is the step log. The executor builds a zip in a partial beside the target and, right
    // after renaming it onto the target, records a `zip_publish` step. So: this operation's partials
    // beside each target always go (a build killed with its process leaves one); the target goes only
    // when a `zip_publish` step says this operation's rename put it there. Any other zip at the path --
    // an old one the user chose to overwrite and the operation never replaced, or one a refusal
    // protected -- stays exactly as it was. What the catalog says about the path is no part of the test.
    // (A crash between the rename and the step leaves the new zip in place: logged, never deleted.)
    let mut seen_zips: HashSet<String> = HashSet::new();
    for f in &files {
        if !seen_zips.insert(f.target_zip_path.clone()) {
            continue;
        }
        let zp = Path::new(&f.target_zip_path);
        let swept = sweep_stale_partials_beside(zp, true);
        if swept > 0 {
            tracing::info!(operation_id, path = %zp.display(), swept, "removed partial zips during rollback");
        }
        if !published_zips.contains(f.target_zip_path.as_str()) {
            if zp.exists() {
                tracing::warn!(operation_id, path = %zp.display(), "zip was not put in place by this operation; left as it is during rollback");
            }
            continue;
        }
        if zp.exists() {
            // Best-effort by design — a locked zip (Windows sharing
            // violation) must not fail the rollback — but never silent.
            if let Err(e) = std::fs::remove_file(zp) {
                tracing::warn!(
                    operation_id,
                    path = %zp.display(),
                    error = %e,
                    "failed to remove the zip this operation published during rollback"
                );
            }
        }
        // Finalize records the zip (`zip_archives`) before the operation completes, so an operation that
        // died in that window leaves a row for a zip that is now gone. A zip that could not be removed
        // keeps its row: it is still there. Also covers a rollback re-run after the file was removed.
        if !zp.exists() {
            crate::archive::restore::forget_zip(conn, &f.target_zip_path)
                .with_context(|| format!("forget the rolled-back zip {}", f.target_zip_path))
                .map_err(|e| {
                    tracing::error!(operation_id, path = %zp.display(), error = %format!("{e:#}"), "rollback could not drop the zip's catalog row");
                    e
                })?;
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

/// True when a file exists at the operation file's source path and hashes (the SAMPLED
/// `compute_xxhash`, the one `expected_hash` was made with) to the catalog's value.
fn source_matches(f: &crate::archive::models::ArchiveOperationFile) -> bool {
    let source = Path::new(&f.source_path);
    if !source.exists() {
        return false;
    }
    match crate::duplicates::compute_xxhash(source) {
        Ok(h) => h == f.expected_hash,
        Err(e) => {
            tracing::warn!(path = %source.display(), error = %e, "rollback: existing source could not be hashed; treated as not restored");
            false
        }
    }
}

/// Copies the staged file onto `target` through a sibling temp: write, fsync, rename. A failure (or a
/// kill) leaves `target` as it was and at worst a stray `.partial` beside it, never a truncated file
/// at the source path. The temp is removed on error.
fn restore_from_staging(staged: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("rollback: create dir {}", parent.display()))?;
    }
    let temp = crate::zip_store::reader::partial_path(target);
    let write = || -> std::io::Result<()> {
        let mut src = std::fs::File::open(staged)?;
        let mut dst = std::fs::File::create(&temp)?;
        std::io::copy(&mut src, &mut dst)?;
        dst.sync_all()
    };
    if let Err(e) = write() {
        if let Err(re) = std::fs::remove_file(&temp) {
            if re.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %temp.display(), error = %re, "rollback: could not remove the temp copy");
            }
        }
        return Err(anyhow::Error::new(e)
            .context(format!("rollback: copy {} -> {}", staged.display(), target.display())));
    }
    if let Err(e) = std::fs::rename(&temp, target) {
        if let Err(re) = std::fs::remove_file(&temp) {
            tracing::warn!(path = %temp.display(), error = %re, "rollback: could not remove the temp copy");
        }
        return Err(anyhow::Error::new(e)
            .context(format!("rollback: rename {} -> {}", temp.display(), target.display())));
    }
    Ok(())
}

/// Files with a step of `stage` in any status.
fn file_ids_with_any_step(
    conn: &Connection,
    operation_id: i64,
    stage: ArchiveStage,
) -> Result<HashSet<i64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT operation_file_id
         FROM archive_operation_steps
         WHERE operation_id = ?1 AND stage = ?2 AND operation_file_id IS NOT NULL",
    )?;
    let rows: Vec<i64> = stmt
        .query_map(rusqlite::params![operation_id, stage.as_str()], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().collect())
}

/// Files with a Done step of `stage`.
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
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    /// Records every emitted `(event_name, payload)` pair so tests can assert
    /// on the wire names the frontend actually listens to.
    #[derive(Default)]
    struct CapturingEmitter(Mutex<Vec<(String, serde_json::Value)>>);

    impl ProgressEmitter for CapturingEmitter {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((event_name.to_string(), payload));
        }
    }

    /// Builds the same fixture as the executor tests.
    fn fixture() -> (Connection, TempDir, TempDir, i64) {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();

        let l1 = scan.path().join("M31/L_001.fits");
        let l2 = scan.path().join("M31/L_002.fits");
        // Three files, so a partially-deleted batch can leave a gap that a
        // position-derived progress counter would get wrong.
        let l3 = scan.path().join("M31/L_003.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        std::fs::write(&l1, b"light-1").unwrap();
        std::fs::write(&l2, b"light-2").unwrap();
        std::fs::write(&l3, b"light-3").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        for (file_id, path, frame_id) in
            [(1000i64, &l1, 10000i64), (1001, &l2, 10001), (1002, &l3, 10002)]
        {
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
        assert!(scan.path().join("M31/L_003.fits").exists());
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

    /// Drives the fixture up to "sources deleted, staging populated" so a
    /// rollback has restore work to do (and therefore emits progress).
    ///
    /// `skip_leading` files are left untouched — no staging copy, no deletion,
    /// no Done DeleteSource step. That is the normal interrupted-archive shape:
    /// the operation died partway through `delete_sources_phase`, so only part
    /// of the batch needs restoring. Returns how many files were staged, which
    /// is the `total` every progress event must carry.
    fn stage_deleted_sources(
        conn: &Connection,
        arch: &TempDir,
        op_id: i64,
        skip_leading: usize,
    ) -> usize {
        let files = adb::list_operation_files(conn, op_id).unwrap();
        crate::archive::staging::ensure_staging_dir(arch.path(), op_id).unwrap();
        for f in files.iter().skip(skip_leading) {
            crate::archive::staging::copy_into_staging(
                arch.path(),
                op_id,
                std::path::Path::new(&f.source_path),
                &f.target_path_in_zip,
            )
            .unwrap();
            std::fs::remove_file(&f.source_path).unwrap();
            let sid =
                adb::insert_step(conn, op_id, Some(f.id), ArchiveStage::DeleteSource).unwrap();
            adb::update_step(conn, sid, StepStatus::Done, None, None).unwrap();
        }
        files.len() - skip_leading
    }

    /// The `(current, total)` pairs of every `archive-progress` event, in order.
    fn progress_counters(events: &[(String, serde_json::Value)]) -> Vec<(u64, u64)> {
        events
            .iter()
            .filter(|(n, _)| n == "archive-progress")
            .map(|(_, p)| (p["current"].as_u64().unwrap(), p["total"].as_u64().unwrap()))
            .collect()
    }

    #[test]
    fn rollback_emits_unified_progress_and_finished_events() {
        let (conn, arch, _scan, op_id) = fixture();
        // Leave the first file alone: an interrupted operation deletes a
        // prefix of the batch, so the restore loop starts partway in.
        let expected_total = stage_deleted_sources(&conn, &arch, op_id, 1);

        let emitter = CapturingEmitter::default();
        rollback_operation_standalone(&conn, op_id, &emitter).unwrap();

        let events = emitter.0.lock().unwrap();
        assert!(
            events.iter().any(|(n, _)| n == "archive-progress"),
            "rollback must emit on the channel the UI listens to: {events:?}"
        );
        // The counter must count restored files, not positions in the batch —
        // deriving it from the loop index reports e.g. "3/2" (150%) whenever
        // the operation died partway through deleting sources.
        let counters = progress_counters(&events);
        let expected: Vec<(u64, u64)> = (1..=expected_total as u64)
            .map(|c| (c, expected_total as u64))
            .collect();
        assert_eq!(counters, expected, "progress counters: {events:?}");
        assert!(counters.iter().all(|(c, t)| c >= &1 && c <= t));
        assert_eq!(counters.last().unwrap().0, expected_total as u64);

        let (_, fin) = events
            .iter()
            .find(|(n, _)| n == "archive-finished")
            .expect("finished event");
        assert_eq!(fin["operation_id"], op_id);
        assert_eq!(fin["outcome"], "completed");
        assert_eq!(fin["kind"], "rollback");
        assert!(
            events.iter().all(|(n, _)| n != "archive-rollback-progress"),
            "the unlistened legacy channel must be gone: {events:?}"
        );
    }

    /// A copy-disposition file gets a Done DeleteSource step too (nothing was
    /// deleted for it — see `executor::delete_sources_phase`), so it must not
    /// inflate the denominator of a rollback that never touches it.
    #[test]
    fn rollback_progress_ignores_copy_disposition_files() {
        let (conn, arch, _scan, op_id) = fixture();
        let staged = stage_deleted_sources(&conn, &arch, op_id, 0);
        // Turn the last file into a copied calibration after the fact: its
        // source stays on disk, its DeleteSource step is Done all the same.
        let files = adb::list_operation_files(&conn, op_id).unwrap();
        let copied = files.last().unwrap();
        conn.execute(
            "UPDATE archive_operation_files SET disposition = 'copy' WHERE id = ?1",
            [copied.id],
        )
        .unwrap();

        let emitter = CapturingEmitter::default();
        rollback_operation(&conn, op_id, &emitter).unwrap();

        let events = emitter.0.lock().unwrap();
        let counters = progress_counters(&events);
        let expected_total = (staged - 1) as u64;
        assert_eq!(
            counters,
            (1..=expected_total).map(|c| (c, expected_total)).collect::<Vec<_>>(),
            "copied file must not be counted: {events:?}"
        );
    }

    /// A rollback on the failure path is a sub-step of a failed operation, not
    /// its terminal state — the caller decides what the operation ended as, so
    /// the inner rollback must not emit a terminal event of its own.
    #[test]
    fn inner_rollback_emits_no_finished_event() {
        let (conn, arch, _scan, op_id) = fixture();
        stage_deleted_sources(&conn, &arch, op_id, 0);

        let emitter = CapturingEmitter::default();
        rollback_operation(&conn, op_id, &emitter).unwrap();

        let events = emitter.0.lock().unwrap();
        assert!(events.iter().any(|(n, _)| n == "archive-progress"));
        assert!(
            events.iter().all(|(n, _)| n != "archive-finished"),
            "inner rollback must not emit a terminal event: {events:?}"
        );
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

    fn target_zip(conn: &Connection, op_id: i64) -> String {
        adb::list_operation_files(conn, op_id).unwrap()[0].target_zip_path.clone()
    }

    #[test]
    fn rollback_leaves_a_pre_existing_zip_the_operation_never_reached() {
        let (conn, _arch, _scan, op_id) = fixture();
        // Overwrite of a zip no row points at; the operation is cancelled before the zip stage.
        let zip = target_zip(&conn, op_id);
        std::fs::write(&zip, b"pre-existing zip bytes").unwrap();
        let cancel = Arc::new(AtomicBool::new(true));
        let _ = run_operation(&conn, op_id, &cancel, &NullEmitter);

        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        assert_eq!(std::fs::read(&zip).unwrap(), b"pre-existing zip bytes", "a cancel leaves things as they were");
        assert_eq!(adb::get_operation(&conn, op_id).unwrap().status, "rolled_back");
    }

    /// Runs the forward phases up to (not including) `delete_sources`, with the zip published.
    fn run_through_zip(conn: &Connection, arch: &TempDir, op_id: i64) -> Vec<crate::archive::models::ArchiveOperationFile> {
        let cancel = Arc::new(AtomicBool::new(false));
        let files = adb::list_operation_files(conn, op_id).unwrap();
        let root = arch.path();
        crate::archive::staging::ensure_staging_dir(root, op_id).unwrap();
        crate::archive::executor::test_phases::copy_and_zip(conn, op_id, &files, root, &cancel).unwrap();
        files
    }

    /// Registers the rows a finished finalize would have written, so "unmarks nothing" is observable.
    fn mark_rows_archived(conn: &Connection, op_id: i64) {
        for f in adb::list_operation_files(conn, op_id).unwrap() {
            adb::mark_file_archived(conn, f.file_id.unwrap(), op_id, &f.target_zip_path, &f.target_path_in_zip).unwrap();
        }
    }

    fn archived_row_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM files WHERE archive_zip_path IS NOT NULL", [], |r| r.get(0)).unwrap()
    }

    /// Fake the delete_sources phase: sources gone, a Done DeleteSource step each.
    fn delete_sources(conn: &Connection, op_id: i64) {
        for f in adb::list_operation_files(conn, op_id).unwrap() {
            std::fs::remove_file(&f.source_path).unwrap();
            let sid = adb::insert_step(conn, op_id, Some(f.id), ArchiveStage::DeleteSource).unwrap();
            adb::update_step(conn, sid, StepStatus::Done, None, None).unwrap();
        }
    }

    #[test]
    fn rollback_leaves_a_zip_whose_rename_this_operation_never_completed_even_with_zip_add_steps_and_stale_rows() {
        let (conn, _arch, _scan, op_id) = fixture();
        let zip = target_zip(&conn, op_id);
        // Stale rows of an earlier zip at this path (the file was missing when the plan was checked).
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, archive_zip_path, archive_path_in_zip)
             VALUES (900, ?1, 'X.fits', 1, '2025-01-01T00:00:00Z', 'FITS', ?2, 'X.fits')",
            params![format!("{zip}!/X.fits"), zip],
        ).unwrap();
        // The zip at the path is an older one. The operation's build started (ZipAdd in progress) and failed.
        std::fs::write(&zip, b"older zip").unwrap();
        let first = adb::list_operation_files(&conn, op_id).unwrap()[0].id;
        let sid = adb::insert_step(&conn, op_id, Some(first), ArchiveStage::ZipAdd).unwrap();
        adb::update_step(&conn, sid, StepStatus::InProgress, None, None).unwrap();

        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        assert_eq!(std::fs::read(&zip).unwrap(), b"older zip", "no rename of this operation: the zip is not ours to delete");
    }

    #[test]
    fn rollback_removes_the_zip_it_published_even_when_stale_rows_point_at_the_path() {
        let (conn, _arch, _scan, op_id) = fixture();
        let zip = target_zip(&conn, op_id);
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, archive_zip_path, archive_path_in_zip)
             VALUES (900, ?1, 'X.fits', 1, '2025-01-01T00:00:00Z', 'FITS', ?2, 'X.fits')",
            params![format!("{zip}!/X.fits"), zip],
        ).unwrap();
        std::fs::write(&zip, b"zip this op renamed into place").unwrap();
        let first = adb::list_operation_files(&conn, op_id).unwrap()[0].id;
        let sid = adb::insert_step(&conn, op_id, Some(first), ArchiveStage::ZipPublish).unwrap();
        adb::update_step(&conn, sid, StepStatus::Done, None, None).unwrap();

        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        assert!(!Path::new(&zip).exists(), "the zip this operation published is removed");
    }

    /// Finalize records the zip before it marks the frame set; an operation that died between the two and is
    /// rolled back must not leave the catalog listing a zip that no longer exists.
    #[test]
    fn rollback_forgets_the_zip_row_and_released_entries_of_the_zip_it_deletes() {
        let (conn, arch, _scan, op_id) = fixture();
        run_through_zip(&conn, &arch, op_id);
        let zip = target_zip(&conn, op_id);
        assert!(Path::new(&zip).exists());
        conn.execute(
            "INSERT INTO zip_archives (path, size, modified_at, frame_entries, scanned_at, created_by_operation)
             VALUES (?1, 10, '2025-01-01T00:00:00Z', 3, '2025-01-01T00:00:00Z', ?2)",
            params![zip, op_id],
        )
        .unwrap();
        conn.execute("INSERT INTO zip_released_entries (zip_id, entry_path) SELECT id, 'x.fits' FROM zip_archives", []).unwrap();
        // A zip of another operation is not touched.
        conn.execute(
            "INSERT INTO zip_archives (path, size, modified_at, frame_entries, scanned_at) VALUES ('/other.zip', 1, '', 1, '')",
            [],
        )
        .unwrap();

        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        assert!(!Path::new(&zip).exists(), "the published zip is removed");
        let rows: Vec<String> = conn
            .prepare("SELECT path FROM zip_archives")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows, vec!["/other.zip".to_string()], "no row for the deleted zip");
        let released: i64 = conn.query_row("SELECT COUNT(*) FROM zip_released_entries", [], |r| r.get(0)).unwrap();
        assert_eq!(released, 0, "its released entries go with it");
    }

    #[test]
    fn rollback_keeps_the_zip_row_of_a_zip_it_did_not_publish() {
        let (conn, _arch, _scan, op_id) = fixture();
        let zip = target_zip(&conn, op_id);
        std::fs::write(&zip, b"older zip").unwrap();
        conn.execute(
            "INSERT INTO zip_archives (path, size, modified_at, frame_entries, scanned_at) VALUES (?1, 9, '', 1, '')",
            params![zip],
        )
        .unwrap();
        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        assert_eq!(std::fs::read(&zip).unwrap(), b"older zip");
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM zip_archives", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1, "a zip the operation never published keeps its row");
    }

    #[test]
    fn rollback_removes_this_operations_partials_and_only_those() {
        let (conn, _arch, _scan, op_id) = fixture();
        let zip = target_zip(&conn, op_id);
        std::fs::write(&zip, b"older zip").unwrap();
        let own = format!("{zip}.{}-7.partial", std::process::id());
        let dead = format!("{zip}.2000000000-1.partial"); // no such process
        #[cfg(unix)]
        let live = format!("{zip}.{}-3.partial", std::os::unix::process::parent_id());
        let users = format!("{zip}.partial");
        let other_name = format!("{zip}x.{}-1.partial", std::process::id());
        for p in [&own, &dead, &users, &other_name] {
            std::fs::write(p, b"partial").unwrap();
        }
        #[cfg(unix)]
        std::fs::write(&live, b"partial").unwrap();

        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        assert!(!Path::new(&own).exists(), "this process's partial goes");
        assert!(!Path::new(&dead).exists(), "a dead process's partial goes");
        #[cfg(unix)]
        assert!(Path::new(&live).exists(), "a running process's partial stays");
        assert!(Path::new(&users).exists(), "a name outside the pattern is not ours");
        assert!(Path::new(&other_name).exists(), "another zip's partial is not ours");
        assert_eq!(std::fs::read(&zip).unwrap(), b"older zip");
    }

    #[test]
    fn a_published_zip_is_removed_and_the_sources_come_back() {
        let (conn, arch, scan, op_id) = fixture();
        run_through_zip(&conn, &arch, op_id);
        let zip = target_zip(&conn, op_id);
        assert!(Path::new(&zip).exists());
        delete_sources(&conn, op_id);
        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        assert!(!Path::new(&zip).exists());
        assert!(scan.path().join("M31/L_001.fits").exists());
        assert_eq!(adb::get_operation(&conn, op_id).unwrap().status, "rolled_back");
    }

    #[test]
    fn rollback_refuses_when_staging_is_gone_after_the_sources_were_deleted() {
        // Interrupted in Finalizing after cleanup_staging: the zip is the frames' only copy.
        let (conn, arch, scan, op_id) = fixture();
        run_through_zip(&conn, &arch, op_id);
        delete_sources(&conn, op_id);
        mark_rows_archived(&conn, op_id);
        crate::archive::staging::cleanup_staging(arch.path(), op_id).unwrap();
        adb::update_operation_status(&conn, op_id, ArchiveStatus::Finalizing, None).unwrap();
        let zip = target_zip(&conn, op_id);
        let zip_bytes = std::fs::read(&zip).unwrap();
        let steps_before: i64 = conn.query_row("SELECT COUNT(*) FROM archive_operation_steps", [], |r| r.get(0)).unwrap();

        let err = rollback_operation(&conn, op_id, &NullEmitter).unwrap_err();
        assert!(format!("{err:#}").contains("nothing was changed"), "{err:#}");
        assert_eq!(std::fs::read(&zip).unwrap(), zip_bytes, "the zip is untouched");
        assert_eq!(archived_row_count(&conn), 3, "no row is unmarked");
        assert_eq!(adb::get_operation(&conn, op_id).unwrap().status, "finalizing", "the status is not changed");
        assert!(!scan.path().join("M31/L_001.fits").exists());
        let steps_after: i64 = conn.query_row("SELECT COUNT(*) FROM archive_operation_steps", [], |r| r.get(0)).unwrap();
        assert_eq!(steps_after, steps_before, "no step is written");
        assert!(adb::list_unfinished_operations(&conn).unwrap().iter().any(|o| o.id == op_id), "still offered for resume");
    }

    #[test]
    fn rollback_refuses_when_the_archive_drive_is_offline() {
        // Sources deleted, the archive folder (staging and zip) is not reachable.
        let (conn, arch, _scan, op_id) = fixture();
        run_through_zip(&conn, &arch, op_id);
        delete_sources(&conn, op_id);
        mark_rows_archived(&conn, op_id);
        adb::update_operation_status(&conn, op_id, ArchiveStatus::DeletingSources, None).unwrap();
        let offline = arch.path().with_extension("offline");
        std::fs::rename(arch.path(), &offline).unwrap();

        let err = rollback_operation(&conn, op_id, &NullEmitter).unwrap_err();
        assert!(format!("{err:#}").contains("neither intact on disk nor in staging"), "{err:#}");
        assert_eq!(archived_row_count(&conn), 3, "nothing is unmarked");
        assert_eq!(adb::get_operation(&conn, op_id).unwrap().status, "deleting_sources");

        // The drive comes back: the same rollback now restores everything.
        std::fs::rename(&offline, arch.path()).unwrap();
        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        assert_eq!(archived_row_count(&conn), 0);
        assert!(!Path::new(&target_zip(&conn, op_id)).exists());
    }

    #[test]
    fn rolling_back_a_rolled_back_operation_leaves_a_later_operations_zip() {
        let (conn, arch, _scan, op_id) = fixture();
        run_through_zip(&conn, &arch, op_id);
        let zip = target_zip(&conn, op_id);
        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        assert!(!Path::new(&zip).exists());
        // A later operation completes a zip at the same path.
        std::fs::write(&zip, b"later operation's finished zip").unwrap();

        let err = rollback_operation(&conn, op_id, &NullEmitter).unwrap_err();
        assert!(format!("{err:#}").contains("already rolled back"), "{err:#}");
        assert_eq!(std::fs::read(&zip).unwrap(), b"later operation's finished zip");
        assert_eq!(adb::get_operation(&conn, op_id).unwrap().status, "rolled_back");
    }

    #[test]
    fn a_failed_build_leaves_an_existing_zip_byte_identical_through_rollback() {
        let (conn, arch, _scan, op_id) = fixture();
        let zip = target_zip(&conn, op_id);
        std::fs::write(&zip, b"older zip that must survive").unwrap();
        let files = adb::list_operation_files(&conn, op_id).unwrap();
        crate::archive::staging::ensure_staging_dir(arch.path(), op_id).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        // The copy phase ran; the staged copy of the last file then vanished before the zip stage.
        crate::archive::executor::test_phases::copy_only(&conn, op_id, &files, arch.path(), &cancel).unwrap();
        let last = files.last().unwrap();
        std::fs::remove_file(crate::archive::staging::staging_file_path(arch.path(), op_id, &last.target_path_in_zip)).unwrap();

        let err = crate::archive::executor::test_phases::zip_only(&conn, op_id, &files, arch.path(), &cancel).unwrap_err();
        assert!(format!("{err:#}").contains("zip build failed"), "{err:#}");
        assert_eq!(std::fs::read(&zip).unwrap(), b"older zip that must survive", "the old zip is untouched by a failed build");
        assert_no_partials(Path::new(&zip));

        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        assert_eq!(std::fs::read(&zip).unwrap(), b"older zip that must survive");
        assert_no_partials(Path::new(&zip));
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_target_directory_fails_the_build_and_leaves_the_old_zip_byte_identical() {
        use std::os::unix::fs::PermissionsExt;
        let (conn, arch, _scan, op_id) = fixture();
        let zip = target_zip(&conn, op_id);
        std::fs::write(&zip, b"older zip that must survive").unwrap();
        let dir = Path::new(&zip).parent().unwrap().to_path_buf();
        let files = adb::list_operation_files(&conn, op_id).unwrap();
        crate::archive::staging::ensure_staging_dir(arch.path(), op_id).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        crate::archive::executor::test_phases::copy_only(&conn, op_id, &files, arch.path(), &cancel).unwrap();

        let original = std::fs::metadata(&dir).unwrap().permissions();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let writable = std::fs::File::create(dir.join("probe")).is_ok(); // root ignores the mode
        let result = crate::archive::executor::test_phases::zip_only(&conn, op_id, &files, arch.path(), &cancel);
        let rolled = rollback_operation(&conn, op_id, &NullEmitter);
        std::fs::set_permissions(&dir, original).unwrap();
        let _ = std::fs::remove_file(dir.join("probe"));
        if writable {
            return;
        }

        assert!(result.is_err(), "the partial cannot be created");
        rolled.unwrap();
        assert_eq!(std::fs::read(&zip).unwrap(), b"older zip that must survive");
    }

    #[test]
    fn until_the_rename_the_old_zip_is_untouched_and_the_build_sits_in_a_partial() {
        use crate::archive::zip_writer::{build_zip_with_progress, ZipEntry};
        let tmp = TempDir::new().unwrap();
        let zip = tmp.path().join("out.zip");
        std::fs::write(&zip, b"old zip").unwrap();
        let mut entries = Vec::new();
        for i in 0..3 {
            let src = tmp.path().join(format!("s{i}"));
            std::fs::write(&src, vec![b'x'; 1000]).unwrap();
            entries.push(ZipEntry { source_path: src, path_in_zip: format!("e{i}") });
        }
        let seen = std::cell::Cell::new(0);
        build_zip_with_progress(&zip, &entries, ArchiveCompression::Store, Some(&|done, _| {
            assert_eq!(std::fs::read(&zip).unwrap(), b"old zip", "after entry {done} the target is still the old zip");
            assert_eq!(partials_beside(&zip).len(), 1, "the build is in one partial");
            seen.set(done);
        })).unwrap();
        assert_eq!(seen.get(), 3);
        assert_no_partials(Path::new(&zip));
        assert_eq!(zip::ZipArchive::new(std::fs::File::open(&zip).unwrap()).unwrap().len(), 3);
    }

    fn partials_beside(zip: &Path) -> Vec<PathBuf> {
        let prefix = format!("{}.", zip.file_name().unwrap().to_string_lossy());
        std::fs::read_dir(zip.parent().unwrap()).unwrap().flatten().map(|e| e.path())
            .filter(|p| {
                let n = p.file_name().unwrap().to_string_lossy().into_owned();
                n.starts_with(&prefix) && n.ends_with(".partial")
            })
            .collect()
    }

    fn assert_no_partials(zip: &Path) {
        assert!(partials_beside(zip).is_empty(), "partials left: {:?}", partials_beside(zip));
    }

    #[test]
    fn rollback_of_a_completed_operation_is_refused() {
        let (conn, _arch, _scan, op_id) = fixture();
        adb::update_operation_status(&conn, op_id, ArchiveStatus::Completed, None).unwrap();
        let zip = target_zip(&conn, op_id);
        std::fs::write(&zip, b"finished zip").unwrap();
        let err = rollback_operation(&conn, op_id, &NullEmitter).unwrap_err();
        assert!(format!("{err:#}").contains("use Unarchive"), "{err:#}");
        assert_eq!(adb::get_operation(&conn, op_id).unwrap().status, "completed");
        assert_eq!(std::fs::read(&zip).unwrap(), b"finished zip");
    }

    #[test]
    fn a_source_removed_before_its_delete_step_was_marked_done_is_restored_from_staging() {
        // Crash (or a failed status write) between remove_file(source) and the Done update: the step is
        // still Pending, the source is gone, and staging holds the only copy besides the zip.
        let (conn, arch, scan, op_id) = fixture();
        run_through_zip(&conn, &arch, op_id);
        let files = adb::list_operation_files(&conn, op_id).unwrap();
        for f in &files[..2] {
            std::fs::remove_file(&f.source_path).unwrap();
            let sid = adb::insert_step(&conn, op_id, Some(f.id), ArchiveStage::DeleteSource).unwrap();
            adb::update_step(&conn, sid, StepStatus::Done, None, None).unwrap();
        }
        let last = &files[2];
        std::fs::remove_file(&last.source_path).unwrap();
        adb::insert_step(&conn, op_id, Some(last.id), ArchiveStage::DeleteSource).unwrap(); // left Pending
        adb::update_operation_status(&conn, op_id, ArchiveStatus::DeletingSources, None).unwrap();
        let zip = target_zip(&conn, op_id);

        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        for name in ["L_001", "L_002", "L_003"] {
            let p = scan.path().join(format!("M31/{name}.fits"));
            assert!(p.exists(), "{} is back", p.display());
        }
        assert_eq!(std::fs::read(scan.path().join("M31/L_003.fits")).unwrap(), b"light-3");
        assert!(!Path::new(&zip).exists(), "the zip goes only after every source is back");
        assert!(!crate::archive::staging::staging_dir(arch.path(), op_id).exists());
        assert_eq!(adb::get_operation(&conn, op_id).unwrap().status, "rolled_back");
    }

    #[test]
    fn a_refused_rollback_after_a_failed_run_leaves_the_operation_listed_as_unfinished() {
        // The archive drive drops after the sources were deleted: the run fails, the rollback is refused
        // (the zip and staging are unreachable), and the banner must still offer Resume / Roll back.
        let (conn, arch, scan, op_id) = fixture();
        run_through_zip(&conn, &arch, op_id);
        delete_sources(&conn, op_id);
        mark_rows_archived(&conn, op_id);
        adb::update_operation_status(&conn, op_id, ArchiveStatus::Finalizing, None).unwrap();
        let offline = arch.path().with_extension("offline");
        std::fs::rename(arch.path(), &offline).unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let err = crate::archive::executor::run_operation_standalone(&conn, op_id, &cancel, &NullEmitter).unwrap_err();
        let op = adb::get_operation(&conn, op_id).unwrap();
        assert!(
            adb::list_unfinished_operations(&conn).unwrap().iter().any(|o| o.id == op_id),
            "status {} is not offered for resume ({err:#})", op.status
        );
        assert!(op.error_message.is_some(), "the failure is kept on the row");
        assert!(!scan.path().join("M31/L_001.fits").exists(), "nothing was changed");
        assert_eq!(archived_row_count(&conn), 3);

        // The drive comes back; the user rolls back from the banner.
        let _ = std::fs::remove_dir_all(arch.path());
        std::fs::rename(&offline, arch.path()).unwrap();
        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        assert!(scan.path().join("M31/L_001.fits").exists());
        assert_eq!(archived_row_count(&conn), 0);
        assert_eq!(adb::get_operation(&conn, op_id).unwrap().status, "rolled_back");
    }

    /// Rewrites each source with 2 MiB of distinct content and sets the row's expected hash to the
    /// SAMPLED catalog hash of it (files this large make a truncation visible to that hash).
    fn make_sources_large(conn: &Connection, op_id: i64) -> std::collections::HashMap<String, Vec<u8>> {
        let mut contents = std::collections::HashMap::new();
        for (i, f) in adb::list_operation_files(conn, op_id).unwrap().iter().enumerate() {
            let bytes: Vec<u8> = (0..2 * 1024 * 1024usize).map(|n| ((n * 31 + i * 7) % 251) as u8).collect();
            std::fs::write(&f.source_path, &bytes).unwrap();
            let h = crate::duplicates::compute_xxhash(Path::new(&f.source_path)).unwrap();
            conn.execute("UPDATE archive_operation_files SET expected_hash = ?1 WHERE id = ?2", params![h, f.id]).unwrap();
            let name = Path::new(&f.source_path).file_name().unwrap().to_str().unwrap().to_string();
            contents.insert(name, bytes);
        }
        contents
    }

    fn leftover_temps(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().ends_with(".partial")).collect()
    }

    #[test]
    fn a_truncated_leftover_at_a_deleted_sources_path_is_replaced_by_the_staged_bytes() {
        // A restore that died halfway (ENOSPC, kill) used to leave a truncated file that a second
        // rollback trusted before it deleted the zip and staging.
        let (conn, arch, scan, op_id) = fixture();
        let contents = make_sources_large(&conn, op_id);
        run_through_zip(&conn, &arch, op_id);
        delete_sources(&conn, op_id);
        adb::update_operation_status(&conn, op_id, ArchiveStatus::RollingBack, None).unwrap();
        let l2 = scan.path().join("M31/L_002.fits");
        std::fs::write(&l2, &contents["L_002.fits"][..1000]).unwrap();
        let zip = target_zip(&conn, op_id);

        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        for name in ["L_001", "L_002", "L_003"] {
            let p = scan.path().join(format!("M31/{name}.fits"));
            assert!(std::fs::read(&p).unwrap() == contents[&format!("{name}.fits")], "{} holds the full staged bytes", p.display());
        }
        assert!(!Path::new(&zip).exists());
        assert!(!crate::archive::staging::staging_dir(arch.path(), op_id).exists());
        assert!(leftover_temps(&scan.path().join("M31")).is_empty(), "no temp copy is left beside the sources");
        assert_eq!(adb::get_operation(&conn, op_id).unwrap().status, "rolled_back");
    }

    #[test]
    fn a_source_that_matches_its_catalog_hash_is_not_copied_again() {
        let (conn, arch, scan, op_id) = fixture();
        let contents = make_sources_large(&conn, op_id);
        let files = run_through_zip(&conn, &arch, op_id);
        // L_001 was never removed but has a DeleteSource step; its staged copy is deliberately different,
        // so a re-copy would show.
        let keep = files.iter().find(|f| f.source_path.ends_with("L_001.fits")).unwrap();
        let sid = adb::insert_step(&conn, op_id, Some(keep.id), ArchiveStage::DeleteSource).unwrap();
        adb::update_step(&conn, sid, StepStatus::Done, None, None).unwrap();
        let staged = crate::archive::staging::staging_file_path(arch.path(), op_id, &keep.target_path_in_zip);
        std::fs::write(&staged, b"tampered staging copy").unwrap();
        for f in files.iter().filter(|f| f.id != keep.id) {
            std::fs::remove_file(&f.source_path).unwrap();
            let sid = adb::insert_step(&conn, op_id, Some(f.id), ArchiveStage::DeleteSource).unwrap();
            adb::update_step(&conn, sid, StepStatus::Done, None, None).unwrap();
        }

        rollback_operation(&conn, op_id, &NullEmitter).unwrap();
        assert!(std::fs::read(scan.path().join("M31/L_001.fits")).unwrap() == contents["L_001.fits"], "the intact source is kept");
        assert!(std::fs::read(scan.path().join("M31/L_002.fits")).unwrap() == contents["L_002.fits"]);
        assert_eq!(adb::get_operation(&conn, op_id).unwrap().status, "rolled_back");
    }

    #[test]
    fn a_truncated_source_with_no_staged_copy_refuses_the_rollback() {
        let (conn, arch, scan, op_id) = fixture();
        let contents = make_sources_large(&conn, op_id);
        run_through_zip(&conn, &arch, op_id);
        delete_sources(&conn, op_id);
        adb::update_operation_status(&conn, op_id, ArchiveStatus::RollingBack, None).unwrap();
        std::fs::write(scan.path().join("M31/L_002.fits"), &contents["L_002.fits"][..1000]).unwrap();
        crate::archive::staging::cleanup_staging(arch.path(), op_id).unwrap();
        let zip = target_zip(&conn, op_id);

        assert!(rollback_operation(&conn, op_id, &NullEmitter).is_err());
        assert!(Path::new(&zip).exists(), "the zip holds the only full copy and stays");
        assert_eq!(std::fs::read(scan.path().join("M31/L_002.fits")).unwrap().len(), 1000, "nothing was changed");
        assert_eq!(adb::get_operation(&conn, op_id).unwrap().status, "rolling_back");
    }
}
