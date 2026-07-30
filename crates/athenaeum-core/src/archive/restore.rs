//! Restore: extract zip(s) into a temp dir, then copy only the *missing*
//! files to their final locations. Files already in place at the catalog's
//! recorded source path are left alone — this preserves both copy-disposition
//! calibrations (whose originals were never deleted) and any user edits made
//! after the archive ran.
//!
//! Stages:
//! - extract:        every entry of every zip → temp dir, hash-verified
//!                   against the plan's expected_hash.
//! - reconcile:      per-file decide whether to skip (already on disk) or
//!                   restore (copy temp → target). "Already on disk" is
//!                   hash-verified against the same expected_hash before
//!                   being trusted — see CONFLICT handling below. Target is
//!                   either the original source_path (preserves layout) or
//!                   `<picked>/<path-in-zip>` for an alternate target.
//! - update_catalog: clear archive markers for restored files; rewrite
//!                   `files.path` only when the restore target differs from
//!                   the original.
//! - cleanup:        delete temp dir, optionally delete zips.
//!
//! CONFLICT handling: a file sitting at `source_path` is only ever trusted
//! as "already restored" if it hashes to the plan's `expected_hash`. Any
//! mismatch (wrong version, different frame, half-written copy) or hash
//! failure is recorded as a conflict — the file is left completely
//! untouched (not overwritten, markers not cleared) and the loop continues
//! so one impostor can't abort the rest of the restore. The zip(s) for an
//! operation that finished with conflicts are kept regardless of
//! `keep_zip_after_restore`, since they're still the only place the correct
//! bytes exist; deleting them would be exactly the data loss this check
//! exists to prevent. The documented remedy is to rename/remove the
//! impostor and re-run restore — reconcile is idempotent and will fill
//! exactly the remaining gaps.

use crate::archive::db as adb;
use crate::archive::models::{ArchiveOperationFile, ArchiveStage, RestoreConflict, RestoreOutcome, StepStatus};
use crate::duplicates::compute_xxhash;
use crate::events::{emit_event, ProgressEmitter};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub type CancelFlag = Arc<AtomicBool>;

#[derive(Serialize, Clone, Debug)]
pub struct RestoreProgress {
    pub operation_id: i64,
    pub stage: String,
    pub current: usize,
    pub total: usize,
    pub message: String,
}

/// Where extracted files should land if they need restoring.
enum RestoreTargetMode<'a> {
    /// Each file restores to its catalog source_path (the location it was
    /// archived from). Reconstructs the original layout exactly.
    Original,
    /// Each file restores to `<root>/<path-in-zip>`. Used when the user
    /// explicitly picked a scan root or custom folder.
    UnderRoot(&'a Path),
}

/// Compute the temp directory used during a restore.
fn restore_temp_dir(archive_root: &Path, operation_id: i64) -> PathBuf {
    archive_root
        .join(".athenaeum_restore_temp")
        .join(format!("op_{}", operation_id))
}

/// Decide whether `target_root_path` represents "restore to original" or
/// "restore under an alternate root". Heuristic: if every plan file's
/// source_path lives under `target_root_path`, the user picked original
/// (or something equivalent that produces the same layout).
fn classify_target<'a>(
    target_root_path: &'a Path,
    files: &[crate::archive::models::ArchiveOperationFile],
) -> RestoreTargetMode<'a> {
    let all_under = files.iter().all(|f| {
        crate::archive::path_layout::path_starts_with_fold(
            Path::new(&f.source_path),
            target_root_path,
        )
    });
    if all_under {
        RestoreTargetMode::Original
    } else {
        RestoreTargetMode::UnderRoot(target_root_path)
    }
}

pub fn run_restore(
    conn: &Connection,
    operation_id: i64,
    target_root_path: &Path,
    overwrite_existing: bool,
    keep_zip_after_restore: bool,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<RestoreOutcome> {
    let op = adb::get_operation(conn, operation_id)?;
    let files = adb::list_operation_files(conn, operation_id)?;
    let total = files.len();
    if total == 0 {
        return Ok(RestoreOutcome::default());
    }

    // Pre-flight: every zip referenced in the plan must exist on disk before
    // we touch the temp dir. Catches the case where the user moved or deleted
    // zip files outside the app, and gives them a clear list of what's missing.
    let unique_zip_paths: HashSet<&str> = files.iter().map(|f| f.target_zip_path.as_str()).collect();
    let mut missing: Vec<&str> = unique_zip_paths
        .iter()
        .filter(|p| !Path::new(p).is_file())
        .copied()
        .collect();
    if !missing.is_empty() {
        missing.sort();
        anyhow::bail!(
            "{} archive zip file{} not found at the expected location — they may have been moved or deleted:\n  {}",
            missing.len(),
            if missing.len() == 1 { "" } else { "s" },
            missing.join("\n  "),
        );
    }

    let archive_root = Path::new(&op.archive_root_path);
    let temp_dir = restore_temp_dir(archive_root, operation_id);
    // Always start with a clean temp dir; previous runs may have left files behind.
    if temp_dir.exists() {
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
    std::fs::create_dir_all(&temp_dir)
        .with_context(|| format!("create temp dir {}", temp_dir.display()))?;

    let mode = classify_target(target_root_path, &files);

    // From here on, ANY error path needs to clean the temp dir. Wrap the rest
    // of the work and intercept Err to cleanup before propagating.
    let result =
        run_restore_inner(conn, operation_id, &op, &files, total, &temp_dir, mode,
                          overwrite_existing, keep_zip_after_restore, cancel, emitter);
    match result {
        Ok(outcome) => Ok(outcome),
        Err(e) => {
            cleanup_temp(&temp_dir);
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_restore_inner(
    conn: &Connection,
    operation_id: i64,
    op: &crate::archive::models::ArchiveOperation,
    files: &[crate::archive::models::ArchiveOperationFile],
    total: usize,
    temp_dir: &Path,
    mode: RestoreTargetMode<'_>,
    overwrite_existing: bool,
    keep_zip_after_restore: bool,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<RestoreOutcome> {

    let emit = |emitter: &dyn ProgressEmitter, stage: &str, current: usize, total: usize, msg: String| {
        let prog = RestoreProgress {
            operation_id,
            stage: stage.to_string(),
            current,
            total,
            message: msg,
        };
        emit_event(emitter, "archive-progress", &prog);
        emit_event(emitter, "archive-restore-progress", &prog);
    };

    // Stage: extract -----------------------------------------------------------
    // Extract every entry into the temp dir, verifying the hash against the plan.
    let mut buf = vec![0u8; 64 * 1024];
    let mut temp_paths: Vec<PathBuf> = Vec::with_capacity(total);

    for (idx, f) in files.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            cleanup_temp(&temp_dir);
            anyhow::bail!("restore cancelled");
        }

        let temp_path = temp_dir.join(&f.target_path_in_zip);
        if let Some(parent) = temp_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }

        let zip_file = File::open(&f.target_zip_path)
            .with_context(|| format!("open zip {}", f.target_zip_path))?;
        let mut archive = zip::ZipArchive::new(BufReader::new(zip_file))
            .with_context(|| format!("parse zip {}", f.target_zip_path))?;
        let mut entry = archive
            .by_name(&f.target_path_in_zip)
            .with_context(|| format!("entry not found in zip: {}", f.target_path_in_zip))?;

        let mut out = File::create(&temp_path)
            .with_context(|| format!("create temp file {}", temp_path.display()))?;
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
        }
        drop(out);
        drop(entry);

        // Verify hash against plan immediately — catches corrupt zips before
        // we proceed to the reconcile step.
        let actual = compute_xxhash(&temp_path)
            .with_context(|| format!("hash {}", temp_path.display()))?;
        if actual != f.expected_hash {
            cleanup_temp(&temp_dir);
            anyhow::bail!(
                "zip integrity check failed for {}: expected {} got {}",
                f.target_path_in_zip, f.expected_hash, actual,
            );
        }
        temp_paths.push(temp_path);

        emit(
            emitter,
            "extract",
            idx + 1,
            total,
            format!("Extracting {}/{}", idx + 1, total),
        );
    }

    // Stage: reconcile ---------------------------------------------------------
    // For each file: skip if already on disk at source_path; else copy temp → target.
    // We track which files actually got restored so the catalog phase only
    // touches those rows.
    let mut restored: Vec<(&crate::archive::models::ArchiveOperationFile, PathBuf)> =
        Vec::with_capacity(total);
    let mut conflicts: Vec<RestoreConflict> = Vec::new();

    for (idx, (f, temp_path)) in files.iter().zip(temp_paths.iter()).enumerate() {
        if cancel.load(Ordering::SeqCst) {
            cleanup_temp(&temp_dir);
            anyhow::bail!("restore cancelled");
        }

        let original = Path::new(&f.source_path);
        let already_in_place = original.is_file() && !overwrite_existing;

        if already_in_place {
            // Don't just trust whatever is sitting at source_path — hash it
            // with the SAME function that produced the plan's expected_hash
            // and compare. Anything else (wrong version, different frame,
            // half-written copy) is a conflict: leave it alone, don't clear
            // its markers, and keep going — see module docs.
            match compute_xxhash(original) {
                Ok(actual) if actual == f.expected_hash => {
                    emit(
                        emitter,
                        "reconcile",
                        idx + 1,
                        total,
                        format!("Already on disk, hash-verified — skipping: {}", f.source_path),
                    );
                }
                Ok(actual) => {
                    let msg = format!(
                        "restore conflict: on-disk file does not match archived hash for {} (expected {}, got {})",
                        f.source_path, f.expected_hash, actual,
                    );
                    tracing::error!(
                        operation_id,
                        file_id = f.file_id.unwrap_or(-1),
                        src = %f.source_path,
                        expected_hash = %f.expected_hash,
                        actual_hash = %actual,
                        "restore conflict: on-disk file hash mismatch, leaving frame set archived pending resolution"
                    );
                    record_conflict_step(conn, operation_id, f, Some(&actual), &msg)?;
                    conflicts.push(RestoreConflict {
                        file_id: f.file_id,
                        source_path: f.source_path.clone(),
                        expected_hash: f.expected_hash.clone(),
                        actual_hash: Some(actual),
                    });
                    emit(
                        emitter,
                        "reconcile",
                        idx + 1,
                        total,
                        format!("CONFLICT — on-disk file doesn't match the archive, left untouched: {}", f.source_path),
                    );
                }
                Err(e) => {
                    // Unreadable is treated exactly like a mismatch: never
                    // panic, never fall through to skip-and-clear.
                    let msg = format!(
                        "restore conflict: could not hash on-disk file at {}: {:#}",
                        f.source_path, e,
                    );
                    tracing::error!(
                        operation_id,
                        file_id = f.file_id.unwrap_or(-1),
                        src = %f.source_path,
                        error = %e,
                        "restore conflict: could not verify on-disk file, leaving frame set archived pending resolution"
                    );
                    record_conflict_step(conn, operation_id, f, None, &msg)?;
                    conflicts.push(RestoreConflict {
                        file_id: f.file_id,
                        source_path: f.source_path.clone(),
                        expected_hash: f.expected_hash.clone(),
                        actual_hash: None,
                    });
                    emit(
                        emitter,
                        "reconcile",
                        idx + 1,
                        total,
                        format!("CONFLICT — could not verify on-disk file, left untouched: {}", f.source_path),
                    );
                }
            }
            continue;
        }

        // Pick the destination path based on mode.
        let dest = match mode {
            RestoreTargetMode::Original => original.to_path_buf(),
            RestoreTargetMode::UnderRoot(root) => {
                crate::archive::path_layout::dest_under_root(root, &f.target_path_in_zip)
            }
        };

        if dest.exists() && !overwrite_existing {
            // The on-disk target is not the original `source_path` (we'd have
            // taken the skip branch above) but something exists there. Don't
            // clobber it.
            emit(
                emitter,
                "reconcile",
                idx + 1,
                total,
                format!("Skipping (target exists): {}", dest.display()),
            );
            continue;
        }

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        std::fs::copy(temp_path, &dest)
            .with_context(|| format!("copy {} -> {}", temp_path.display(), dest.display()))?;

        restored.push((f, dest));
        emit(
            emitter,
            "reconcile",
            idx + 1,
            total,
            format!("Restored {}/{}", idx + 1, total),
        );
    }

    // Stage: update_catalog ----------------------------------------------------
    // For each restored file, sync archive markers + path/mtime/size to match
    // the destination file as it now exists on disk. This is what prevents the
    // scanner's modified-file detection from later wiping these rows.
    //
    // The final "+1" tick below is the frame-set unarchive/keep-archived step
    // — a calibration-set op (Task 14, `op.frames_set_id: None`) has no frame
    // set to touch, so that tick is skipped entirely for it.
    let catalog_total = restored.len() + if op.frames_set_id.is_some() { 1 } else { 0 };
    let mut catalog_done: usize = 0;
    emit(
        emitter,
        "update_catalog",
        catalog_done,
        catalog_total,
        "Updating catalog".into(),
    );

    for (f, new_path) in &restored {
        if let Some(file_id) = f.file_id {
            let meta = std::fs::metadata(new_path)
                .with_context(|| format!("stat restored file {}", new_path.display()))?;
            let new_size = meta.len() as i64;
            let new_mtime = chrono::DateTime::<chrono::Utc>::from(
                meta.modified()
                    .with_context(|| format!("read modified_at for {}", new_path.display()))?
            ).to_rfc3339();

            // Rewrite path only if it differs from the catalog's source_path;
            // mtime/size are always synced (they're the whole point of this fix).
            let path_changed = new_path.to_str() != Some(f.source_path.as_str());
            let path_arg = if path_changed { new_path.to_str() } else { None };
            adb::unmark_file_archived(conn, file_id, path_arg, Some(&new_mtime), Some(new_size))?;
        }
        catalog_done += 1;
        emit(
            emitter,
            "update_catalog",
            catalog_done,
            catalog_total,
            format!("Updating paths ({}/{})", catalog_done, catalog_total),
        );
    }

    // Even when nothing was restored (everything was already on disk), the
    // frame set's archived_at must still be cleared so it returns to active
    // view — but ONLY when every file reconciled cleanly. If any file has an
    // unresolved conflict, the frame set must stay in its archived/zipped
    // state: clearing archived_at here would (a) drop it out of
    // `list_archived_frame_sets`, hiding the "Unarchive" button that is the
    // only UI path back into restore, and (b) make it look like an ordinary
    // active frame set eligible for "Move to Archive" again — re-archiving
    // would zip the conflicted file's unverified on-disk bytes and overwrite
    // the still-intact markers, orphaning the zip that holds the only
    // correct copy. See module docs (CONFLICT handling).
    if let Some(fs_id) = op.frames_set_id {
        if conflicts.is_empty() {
            adb::unmark_frame_set_archived(conn, fs_id)?;
            catalog_done += 1;
            emit(
                emitter,
                "update_catalog",
                catalog_done,
                catalog_total,
                "Frame set unarchived".into(),
            );
        } else {
            tracing::warn!(
                operation_id,
                frame_set_id = fs_id,
                conflicts = conflicts.len(),
                "restore finished with conflicts, frame set stays archived pending resolution"
            );
            catalog_done += 1;
            emit(
                emitter,
                "update_catalog",
                catalog_done,
                catalog_total,
                format!("Frame set stays archived — {} conflict(s) unresolved", conflicts.len()),
            );
        }
    } else if !conflicts.is_empty() {
        // Calibration-set op (Task 14): no frame set to keep/clear — just
        // log. The unresolved conflict itself is still reported to the
        // caller via `RestoreOutcome.conflicts` regardless of subject.
        tracing::warn!(
            operation_id,
            calibration_set_id = ?op.calibration_set_id,
            conflicts = conflicts.len(),
            "calibration archive restore finished with conflicts"
        );
    }

    // For files that weren't restored (skipped because already on disk), still
    // clear any leftover archive markers. Also reconcile modified_at/size with
    // disk if they drifted — this is the recovery path for catalogs that were
    // previously corrupted by an older restore cycle.
    //
    // Conflicted files are excluded from this: their archive markers
    // (archived_in_operation / archive_zip_path / archive_path_in_zip) MUST
    // stay intact so a re-run still sees them as needing reconcile, and we
    // must never bless an unverified on-disk file as "restored".
    let restored_ids: HashSet<i64> = restored
        .iter()
        .filter_map(|(f, _)| f.file_id)
        .collect();
    let conflicted_ids: HashSet<i64> = conflicts
        .iter()
        .filter_map(|c| c.file_id)
        .collect();
    for f in files {
        if let Some(fid) = f.file_id {
            if !restored_ids.contains(&fid) && !conflicted_ids.contains(&fid) {
                // Stat the on-disk file at source_path. If it doesn't exist
                // (file genuinely missing) we still clear markers so the row
                // doesn't keep pointing at an orphaned zip.
                let on_disk = std::fs::metadata(&f.source_path).ok();
                let (mtime_arg, size_arg) = match on_disk {
                    Some(m) => {
                        let mtime = m.modified()
                            .ok()
                            .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());
                        let size = m.len() as i64;
                        (mtime, Some(size))
                    }
                    None => (None, None),
                };
                adb::unmark_file_archived(
                    conn, fid, None,
                    mtime_arg.as_deref(),
                    size_arg,
                )?;
            }
        }
    }

    // Stage: cleanup -----------------------------------------------------------
    cleanup_temp(temp_dir);

    if keep_zip_after_restore {
        // Caller explicitly asked to keep the zip(s); nothing to do.
    } else if !conflicts.is_empty() {
        // At least one file's only verified-correct copy still lives inside
        // a zip from this operation (its on-disk copy failed hash
        // verification). Deleting any zip here could destroy the sole
        // correct copy of a conflicted file before the user has a chance to
        // resolve the conflict and re-run restore. Keep everything.
        tracing::warn!(
            operation_id,
            conflicts = conflicts.len(),
            "keeping archive zip(s) after restore so a re-run can fill the gap(s)"
        );
        emit(
            emitter,
            "cleanup",
            0,
            0,
            format!("Keeping archive zip(s) — {} conflict(s) unresolved", conflicts.len()),
        );
    } else {
        let zip_paths: Vec<String> = {
            let mut seen: HashSet<String> = HashSet::new();
            files
                .iter()
                .filter_map(|f| {
                    if seen.insert(f.target_zip_path.clone()) {
                        Some(f.target_zip_path.clone())
                    } else {
                        None
                    }
                })
                .collect()
        };
        let cleanup_total = zip_paths.len();
        for (idx, zp) in zip_paths.iter().enumerate() {
            let _ = std::fs::remove_file(zp);
            emit(
                emitter,
                "cleanup",
                idx + 1,
                cleanup_total,
                format!("Removing zip {}/{}", idx + 1, cleanup_total),
            );
        }
    }

    Ok(RestoreOutcome { conflicts })
}

/// Record a restore reconcile conflict as a Failed step, reusing the same
/// `archive_operation_steps` machinery the forward archive path uses to
/// record per-file failures. Purely an audit trail — restore itself never
/// consults these rows (its idempotency comes from re-checking disk state
/// on every run), so writing one can never block or alter a re-run.
fn record_conflict_step(
    conn: &Connection,
    operation_id: i64,
    f: &ArchiveOperationFile,
    actual_hash: Option<&str>,
    message: &str,
) -> Result<()> {
    let step_id = adb::insert_step(conn, operation_id, Some(f.id), ArchiveStage::VerifyRestore)?;
    adb::update_step(conn, step_id, StepStatus::Failed, actual_hash, Some(message))?;
    Ok(())
}

fn cleanup_temp(temp_dir: &Path) {
    if temp_dir.exists() {
        let _ = std::fs::remove_dir_all(temp_dir);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::archive::executor::run_operation;
    use crate::archive::models::{ArchiveCompression, ConflictResolution, Dispositions};
    use crate::archive::planner::{build_plan, commit_plan};
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use rusqlite::params;
    use tempfile::TempDir;

    fn op_file(source_path: &str) -> ArchiveOperationFile {
        ArchiveOperationFile {
            id: 0,
            operation_id: 0,
            file_id: None,
            source_path: source_path.to_string(),
            target_zip_path: String::new(),
            target_path_in_zip: String::new(),
            expected_hash: String::new(),
            disposition: "move".to_string(),
            frame_role: "light".to_string(),
            file_size_bytes: 0,
        }
    }

    /// `astro2` is a sibling of `astro`, not a child — a plain string/prefix
    /// compare would call this "restore to original" and write the files back
    /// to a root the user did not pick.
    #[test]
    fn classify_target_rejects_sibling_name_prefix() {
        let files = vec![
            op_file("/photos/astro2/M31/x.fits"),
            op_file("/photos/astro2/M31/y.fits"),
        ];
        assert!(matches!(
            classify_target(Path::new("/photos/astro"), &files),
            RestoreTargetMode::UnderRoot(_)
        ));
    }

    /// On a case-insensitive host `/data/astro` and `/data/Astro` are one
    /// directory, so this IS a restore-to-original.
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn classify_target_folds_case_on_case_insensitive_hosts() {
        let files = vec![
            op_file("/data/astro/M31/x.fits"),
            op_file("/data/astro/M31/y.fits"),
        ];
        assert!(matches!(
            classify_target(Path::new("/data/Astro"), &files),
            RestoreTargetMode::Original
        ));
    }

    /// End-to-end smoke test: archive a single light, restore to an alternate
    /// target, verify the catalog points at the new location.
    #[test]
    fn full_archive_then_restore_cycle() {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();
        let restore_target = TempDir::new().unwrap();

        let l1 = scan.path().join("M31/L_001.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        std::fs::write(&l1, b"original-content").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1000, ?1, 'L_001.fits', 16, '2025-10-12', 'FITS')",
            [l1.to_str().unwrap()],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp)
             VALUES (10000, 1000, 'M31', 'T', 'C', 'Light')",
            [],
        ).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000)", []).unwrap();

        let plan = build_plan(
            &conn, 1, arch.path(),
            &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
            ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();

        assert!(!l1.exists(), "source should have been moved into the zip");

        run_restore(
            &conn, op_id, restore_target.path(),
            false, false, &cancel, &NullEmitter,
        ).unwrap();

        // Zip should be gone (keep_zip_after_restore=false).
        let zips: Vec<_> = std::fs::read_dir(arch.path()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "zip").unwrap_or(false))
            .collect();
        assert_eq!(zips.len(), 0);

        // Catalog path rewritten to the new target.
        let new_path: String = conn.query_row(
            "SELECT path FROM files WHERE id = 1000", [], |r| r.get(0),
        ).unwrap();
        assert!(new_path.starts_with(restore_target.path().to_str().unwrap()));
        let restored_content = std::fs::read(&new_path).unwrap();
        assert_eq!(restored_content, b"original-content");

        // Lock the Task 3 fix: catalog modified_at + size MUST match the
        // freshly-extracted file. Without this sync, a subsequent monitor
        // scan would classify the restored file as 'modified' and DELETE+
        // re-INSERT it, orphaning frames and breaking session_members JOINs.
        let (db_size, db_mtime): (i64, String) = conn.query_row(
            "SELECT size, modified_at FROM files WHERE id = 1000",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        let on_disk_meta = std::fs::metadata(&new_path).unwrap();
        let on_disk_size = on_disk_meta.len() as i64;
        let on_disk_mtime = chrono::DateTime::<chrono::Utc>::from(
            on_disk_meta.modified().unwrap()
        ).to_rfc3339();
        assert_eq!(db_size, on_disk_size, "catalog size must match restored file");
        assert_eq!(db_mtime, on_disk_mtime, "catalog modified_at must match restored file");

        // Frame set marker cleared.
        let archived_at: Option<String> = conn.query_row(
            "SELECT archived_at FROM frames_set WHERE id = 1", [], |r| r.get(0),
        ).unwrap();
        assert!(archived_at.is_none());

        // Temp dir gone.
        let temp = restore_temp_dir(arch.path(), op_id);
        assert!(!temp.exists());
    }

    /// Reconcile bug-fix: archive with a copy-disposition dark, then restore.
    /// The dark is still on disk at its original location, so restore should
    /// SKIP it and not duplicate. Catalog should remain consistent.
    #[test]
    fn restore_skips_copy_disposition_files_already_on_disk() {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();

        let l1 = scan.path().join("M31/L_001.fits");
        let d1 = scan.path().join("Cal/MasterDark.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        std::fs::create_dir_all(d1.parent().unwrap()).unwrap();
        std::fs::write(&l1, b"light-content").unwrap();
        std::fs::write(&d1, b"dark-content-original").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1000, ?1, 'L_001.fits', 13, '2025-10-12', 'FITS'),
                    (2000, ?2, 'MasterDark.fits', 21, '2025-10-10', 'FITS')",
            params![l1.to_str().unwrap(), d1.to_str().unwrap()],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp, is_master)
             VALUES (10000, 1000, 'M31', 'T', 'C', 'Light', 0),
                    (20000, 2000, NULL, 'T', 'C', 'Dark', 1)",
            [],
        ).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000)", []).unwrap();
        conn.execute("INSERT INTO calibration_set (id, imagetyp, date) VALUES (500, 'Dark', '2025-10-10')", []).unwrap();
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (500, 20000)", []).unwrap();
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (10000, 'frame', 500, 'Dark', '2025-10-12')",
            [],
        ).unwrap();

        let plan = build_plan(
            &conn, 1, arch.path(),
            &Dispositions {
                flats: None,
                darks: Some(crate::archive::models::ArchiveDisposition::Copy),
                bias: None,
                darkflats: None,
            },
            ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();

        // Light moved → gone; dark copied → stays.
        assert!(!l1.exists());
        assert!(d1.exists());

        // Restore to original locations.
        let outcome = run_restore(
            &conn, op_id, scan.path(),
            false, false, &cancel, &NullEmitter,
        ).unwrap();

        // Verified-skip path preserves current behavior: the on-disk dark
        // hashes to exactly what the plan recorded, so it's a clean skip —
        // zero conflicts.
        assert!(!outcome.has_conflicts(), "matching on-disk file must not be a conflict: {:?}", outcome.conflicts);

        // Light is back at its original path (was the only one missing).
        assert!(l1.exists());
        let l1_content = std::fs::read(&l1).unwrap();
        assert_eq!(l1_content, b"light-content");

        // Dark is still at its original path with its original content (untouched).
        assert!(d1.exists());
        let d1_content = std::fs::read(&d1).unwrap();
        assert_eq!(d1_content, b"dark-content-original");

        // Catalog: dark's path should still point to its original location, no
        // archive markers leaking through.
        let dark_row: (String, Option<i64>, Option<String>) = conn.query_row(
            "SELECT path, archived_in_operation, archive_zip_path FROM files WHERE id = 2000",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(dark_row.0, d1.to_str().unwrap());
        assert!(dark_row.1.is_none());
        assert!(dark_row.2.is_none());

        // Frame set is no longer archived.
        let archived_at: Option<String> = conn.query_row(
            "SELECT archived_at FROM frames_set WHERE id = 1", [], |r| r.get(0),
        ).unwrap();
        assert!(archived_at.is_none());
    }

    /// The hash-verify fix (W2-T4): a file already sitting at source_path is
    /// only trusted as "already restored" if it hashes to the plan's
    /// expected_hash. An impostor (present, but wrong bytes) must be
    /// reported as a CONFLICT — never silently blessed as restored while the
    /// only correct copy waits inside a zip that could later be deleted.
    /// Also pins the documented remedy: rename/remove the impostor and
    /// re-run — reconcile is idempotent and fills exactly the gap.
    #[test]
    fn restore_reconcile_conflict_then_heals_on_rerun() {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();

        let l1 = scan.path().join("M31/L_001.fits");
        let d1 = scan.path().join("Cal/MasterDark.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        std::fs::create_dir_all(d1.parent().unwrap()).unwrap();
        std::fs::write(&l1, b"original-light-content").unwrap();
        std::fs::write(&d1, b"dark-content-original").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1000, ?1, 'L_001.fits', 22, '2025-10-12', 'FITS'),
                    (2000, ?2, 'MasterDark.fits', 21, '2025-10-10', 'FITS')",
            params![l1.to_str().unwrap(), d1.to_str().unwrap()],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp, is_master)
             VALUES (10000, 1000, 'M31', 'T', 'C', 'Light', 0),
                    (20000, 2000, NULL, 'T', 'C', 'Dark', 1)",
            [],
        ).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000)", []).unwrap();
        conn.execute("INSERT INTO calibration_set (id, imagetyp, date) VALUES (500, 'Dark', '2025-10-10')", []).unwrap();
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (500, 20000)", []).unwrap();
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (10000, 'frame', 500, 'Dark', '2025-10-12')",
            [],
        ).unwrap();

        let plan = build_plan(
            &conn, 1, arch.path(),
            &Dispositions {
                flats: None,
                darks: Some(crate::archive::models::ArchiveDisposition::Copy),
                bias: None,
                darkflats: None,
            },
            ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();

        // Light is move-disposition (always, for LIGHT frames) → deleted from
        // disk and archive markers set on `files`. Copy-disposition dark
        // stays on disk and is never marked (matches
        // `restore_skips_copy_disposition_files_already_on_disk`).
        assert!(!l1.exists(), "light should have been moved into the zip");
        assert!(d1.exists(), "copy-disposition dark stays on disk");
        let light_marker_before: Option<i64> = conn.query_row(
            "SELECT archived_in_operation FROM files WHERE id = 1000", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(light_marker_before, Some(op_id), "forward archive must mark the move-disposition light");

        // Plant an impostor at the light's original path: different bytes
        // than what's in the zip. Simulates a wrong version / different
        // frame / half-written copy sitting where the archived file used to
        // be — e.g. something else wrote there between archive and restore.
        std::fs::write(&l1, b"IMPOSTOR-BYTES-NOT-THE-REAL-LIGHT").unwrap();

        // --- Restore #1: must detect the conflict, not bless the impostor.
        let outcome = run_restore(
            &conn, op_id, scan.path(),
            false, false, &cancel, &NullEmitter,
        ).unwrap();

        assert_eq!(outcome.conflicts.len(), 1, "expected exactly 1 conflict, got: {:?}", outcome.conflicts);
        let conflict = &outcome.conflicts[0];
        assert_eq!(conflict.source_path, l1.to_str().unwrap());
        assert_eq!(conflict.file_id, Some(1000));
        assert_ne!(conflict.actual_hash.as_deref(), Some(conflict.expected_hash.as_str()));

        // (a) the on-disk impostor is byte-identical to what we planted — NOT overwritten.
        let l1_content = std::fs::read(&l1).unwrap();
        assert_eq!(l1_content, b"IMPOSTOR-BYTES-NOT-THE-REAL-LIGHT");

        // (c) archive markers for the conflicted file are NOT cleared.
        let light_row: (Option<i64>, Option<String>, Option<String>) = conn.query_row(
            "SELECT archived_in_operation, archive_zip_path, archive_path_in_zip FROM files WHERE id = 1000",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(light_row.0, Some(op_id), "archived_in_operation must stay intact on conflict");
        assert!(light_row.1.is_some(), "archive_zip_path must stay intact on conflict");
        assert!(light_row.2.is_some(), "archive_path_in_zip must stay intact on conflict");

        // (d) the OTHER file (copy-disposition dark, already correct on disk)
        // restored/skipped normally despite the light's conflict — one
        // impostor must not abort the rest of the restore.
        assert!(d1.exists());
        assert_eq!(std::fs::read(&d1).unwrap(), b"dark-content-original");
        let dark_marker: Option<i64> = conn.query_row(
            "SELECT archived_in_operation FROM files WHERE id = 2000", [], |r| r.get(0),
        ).unwrap();
        assert!(dark_marker.is_none(), "copy-disposition dark was never marked; unaffected either way");

        // The zip(s) must survive a conflicted restore even with
        // keep_zip_after_restore=false — the Lights zip is still the only
        // place the real light bytes exist right now.
        let zips_after_conflict: Vec<_> = std::fs::read_dir(arch.path()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "zip").unwrap_or(false))
            .collect();
        assert!(!zips_after_conflict.is_empty(), "zip(s) must be kept while a conflict is unresolved");

        // (e) the frame set itself must stay archived/zipped while a conflict
        // is unresolved. If `archived_at`/`is_archived` got cleared here, the
        // set would vanish from `list_archived_frame_sets` — which is the
        // ONLY place the "Unarchive" button (the sole UI path back into
        // restore) is rendered — making the documented remedy ("remove the
        // impostor, re-run restore") unreachable from the app. Worse, an
        // un-marked set looks like an ordinary active set eligible for
        // "Move to Archive" again, which would zip the impostor's bytes and
        // orphan the zip holding the real ones.
        let (archived_at_conflicted, is_archived_conflicted): (Option<String>, i64) = conn.query_row(
            "SELECT archived_at, is_archived FROM frames_set WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert!(archived_at_conflicted.is_some(), "frame set must stay archived (zipped) while a conflict is unresolved");
        assert_eq!(is_archived_conflicted, 1, "frame set must stay in the Archive section while a conflict is unresolved");

        // --- Restore #2 (re-run heals): remove the impostor, restore again.
        // The conflicted run above must leave the operation re-runnable
        // through the same public entry point used for restore #1.
        std::fs::remove_file(&l1).unwrap();
        let outcome2 = run_restore(
            &conn, op_id, scan.path(),
            false, false, &cancel, &NullEmitter,
        ).unwrap();

        assert!(!outcome2.has_conflicts(), "conflict must be resolved once the impostor is gone: {:?}", outcome2.conflicts);
        assert!(l1.exists(), "light must be extracted from the zip on the healing run");
        assert_eq!(std::fs::read(&l1).unwrap(), b"original-light-content");

        let light_row_after: (Option<i64>, Option<String>) = conn.query_row(
            "SELECT archived_in_operation, archive_zip_path FROM files WHERE id = 1000",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert!(light_row_after.0.is_none(), "markers must clear once healed");
        assert!(light_row_after.1.is_none());

        // Now that every file reconciled cleanly, the frame set must be
        // unmarked exactly as before this fix.
        let (archived_at_after, is_archived_after): (Option<String>, i64) = conn.query_row(
            "SELECT archived_at, is_archived FROM frames_set WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert!(archived_at_after.is_none(), "frame set must be unarchived once the healing run has zero conflicts");
        assert_eq!(is_archived_after, 0, "frame set must leave the Archive section once healed");
    }

    /// Pre-flight check fires when a zip file referenced by the plan no longer
    /// exists on disk. Restore must abort with a clear error before creating
    /// the temp dir or touching any catalog rows.
    #[test]
    fn restore_aborts_when_zip_is_missing() {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();

        let l1 = scan.path().join("M31/L_001.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        std::fs::write(&l1, b"x").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1000, ?1, 'L_001.fits', 1, '2025-10-12', 'FITS')",
            [l1.to_str().unwrap()],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp)
             VALUES (10000, 1000, 'M31', 'T', 'C', 'Light')",
            [],
        ).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000)", []).unwrap();

        let plan = build_plan(
            &conn, 1, arch.path(),
            &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
            ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();

        // Manually delete the zip to simulate the user moving it away.
        for entry in std::fs::read_dir(arch.path()).unwrap().flatten() {
            if entry.path().extension().and_then(|s| s.to_str()) == Some("zip") {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }

        let restore_target = TempDir::new().unwrap();
        let err = run_restore(
            &conn, op_id, restore_target.path(),
            false, false, &cancel, &NullEmitter,
        ).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("not found"), "expected missing-zip error, got: {}", msg);

        // Catalog must remain untouched.
        let archived_at: Option<String> = conn.query_row(
            "SELECT archived_at FROM frames_set WHERE id = 1", [], |r| r.get(0),
        ).unwrap();
        assert!(archived_at.is_some(), "frame set should still be marked archived");

        // Temp dir must NOT have been created since the bail happened first.
        let temp = restore_temp_dir(arch.path(), op_id);
        assert!(!temp.exists(), "temp dir should not exist after preflight bail");
    }

    /// Pins the M33 data-loss bug: archive (move) → restore → monitor scan
    /// must leave the JOIN chain `session_members → frames → files` intact for
    /// every restored frame. NOTE: FK enforcement is ON by default for this
    /// codebase's connections (rusqlite's bundled sqlite3 is compiled with
    /// SQLITE_DEFAULT_FOREIGN_KEYS=1) — an earlier version of this comment
    /// claimed production runs with `foreign_keys = 0`, which was stale. The
    /// scanner's modified-file branch DELETEs and re-INSERTs `files` with a
    /// fresh id; the JOIN chain breaks under either FK mode (orphaned
    /// `frames.file_id` with FKs off, CASCADE through `frames` into
    /// `session_members` with FKs on), and the assertions below catch both.
    #[test]
    fn archive_then_restore_then_scan_preserves_session_members() {
        use crate::scanner::scan_directory_parallel;
        use crate::events::NullEmitter;
        use std::sync::atomic::AtomicBool;

        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();

        // Use a real FITS file so process_file_parallel can parse it. The
        // simplest synthetic FITS that fitsio accepts is created by writing a
        // minimal SIMPLE/BITPIX/NAXIS=0/END header.
        let l1 = scan.path().join("M33/L_001.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        write_minimal_fits(&l1);

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // This connection matches production: FK enforcement is ON (the
        // bundled sqlite3's SQLITE_DEFAULT_FOREIGN_KEYS=1 default — nothing
        // in this codebase turns it off). An earlier version of this comment
        // claimed production runs with foreign_keys=0, "verified" on the
        // live DB via the sqlite3 CLI — but the pragma is per-connection,
        // not a DB property, and the CLI's own connections default to OFF.
        // Either FK mode kills the JOIN chain when the scanner DELETEs and
        // re-INSERTs the files row with fresh IDs: with FKs off the original
        // frames row is orphaned pointing at a now-missing files.id; with
        // FKs on the DELETE cascades through frames into session_members.
        // Both leave the restored set empty in the UI, and the assertions
        // below catch both.
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M33', 1)", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();

        let l1_size = std::fs::metadata(&l1).unwrap().len() as i64;
        let l1_mtime = chrono::DateTime::<chrono::Utc>::from(
            std::fs::metadata(&l1).unwrap().modified().unwrap()
        ).to_rfc3339();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1000, ?1, 'L_001.fits', ?2, ?3, 'FITS')",
            params![l1.to_str().unwrap(), l1_size, l1_mtime],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp)
             VALUES (10000, 1000, 'M33', 'T', 'C', 'Light')",
            [],
        ).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000)", []).unwrap();

        // Archive (move).
        let plan = build_plan(
            &conn, 1, arch.path(),
            &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
            ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();
        assert!(!l1.exists(), "source should have been moved into the zip");

        // Restore.
        run_restore(
            &conn, op_id, scan.path(),
            false, false, &cancel, &NullEmitter,
        ).unwrap();
        assert!(l1.exists(), "file should have been restored");

        let cancel2 = Arc::new(AtomicBool::new(false));
        let scan_result = scan_directory_parallel(
            scan.path(), 1, &conn, &NullEmitter,
            false, cancel2, false,
        );
        assert!(
            scan_result.errors.is_empty(),
            "scan must succeed without errors, got: {:?}",
            scan_result.errors,
        );
        assert!(!scan_result.cancelled, "scan must not be cancelled");

        // The whole point of the test: a monitor scan after restore must NOT
        // strip the frame's reachability through session_members. Express this
        // as a JOIN through the chain the UI relies on. With FKs off (production
        // default), the bug manifests as orphaning: the original `frames` row
        // points at a now-deleted `files` row, and the new `frames` row from
        // re-insert has no session_members. Either way, this JOIN should still
        // return exactly one matching row when the catalog is healthy.
        let joined_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM session_members sm
             JOIN frames f ON f.id = sm.frame_id
             JOIN files fi ON fi.id = f.file_id
             WHERE sm.session_id = 100 AND fi.path = ?1",
            [l1.to_str().unwrap()], |r| r.get(0),
        ).unwrap();
        assert_eq!(
            joined_count, 1,
            "frame must remain reachable through session_members → frames → files \
             JOIN after restore + scan (production runs with FKs off; orphaning is \
             the actual failure mode)",
        );

        // The catalog's view of (size, modified_at) must match disk so the next
        // scan classifies the file as unchanged.
        let (db_size, db_mtime): (i64, String) = conn.query_row(
            "SELECT size, modified_at FROM files WHERE path = ?1",
            [l1.to_str().unwrap()], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        let on_disk_size = std::fs::metadata(&l1).unwrap().len() as i64;
        let on_disk_mtime = chrono::DateTime::<chrono::Utc>::from(
            std::fs::metadata(&l1).unwrap().modified().unwrap()
        ).to_rfc3339();
        assert_eq!(db_size, on_disk_size);
        assert_eq!(db_mtime, on_disk_mtime);
    }

    /// Minimal FITS file the in-house parser can read. Empty primary HDU.
    /// Each card is exactly 80 bytes; the header block is padded to 2880.
    pub(crate) fn write_minimal_fits(path: &Path) {
        fn card(s: &str) -> [u8; 80] {
            assert!(s.len() <= 80, "card too long: {:?} ({} bytes)", s, s.len());
            let mut out = [b' '; 80];
            out[..s.len()].copy_from_slice(s.as_bytes());
            out
        }

        let cards: [[u8; 80]; 8] = [
            card("SIMPLE  =                    T / file conforms to FITS standard"),
            card("BITPIX  =                    8 / number of bits per data pixel"),
            card("NAXIS   =                    0 / number of data axes"),
            card("OBJECT  = 'M33     '           / target name"),
            card("TELESCOP= 'T       '"),
            card("INSTRUME= 'C       '"),
            card("IMAGETYP= 'Light   '"),
            card("END"),
        ];

        let mut header: Vec<u8> = Vec::with_capacity(2880);
        for c in &cards {
            header.extend_from_slice(c);
        }
        while header.len() < 2880 {
            header.push(b' ');
        }
        std::fs::write(path, header).unwrap();
    }
}
