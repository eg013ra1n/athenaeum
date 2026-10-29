//! Restore: extract only the *missing* files from the zip(s) straight to their
//! final locations. Files already in place at the catalog's recorded source
//! path are left alone — this preserves both copy-disposition calibrations
//! (whose originals were never deleted) and any user edits made after the
//! archive ran. There is no temp tree: an entry is extracted only when its
//! target is free, through `zip_store::place::place_entry`, which never
//! replaces a file.
//!
//! Stages (per file, in one pass):
//! - reconcile:      decide whether to skip (already on disk) or restore.
//!                   "Already on disk" is hash-verified against the plan's
//!                   expected_hash before being trusted — see CONFLICT
//!                   handling below. Target is either the original
//!                   source_path (preserves layout) or
//!                   `<picked>/<path-in-zip>` for an alternate target.
//! - extract:        a missing target is extracted from its zip and the
//!                   placed file is hash-verified against expected_hash; a
//!                   mismatch removes what this run placed and is a conflict.
//! - update_catalog: clear archive markers for restored files; rewrite
//!                   `files.path` only when the restore target differs from
//!                   the original. With `keep_zip_after_restore` each
//!                   restored entry is recorded in `zip_released_entries`.
//! - cleanup:        optionally delete zips — never one that still has
//!                   catalog rows living in it.
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
//! exactly the remaining gaps. When restoring under another folder, only the frame's own
//! bytes at `source_path` count as "already restored"; a different file there (a newer capture
//! that reused the name) is not in the way.

use crate::archive::db as adb;
use crate::archive::models::{ArchiveOperationFile, ArchiveStage, RestoreConflict, RestoreOutcome, StepStatus};
use crate::duplicates::compute_xxhash;
use crate::events::{emit_event, ProgressEmitter};
use crate::zip_store::place::{place_entry, record_released_entry, PlaceOutcome};
use crate::zip_store::reader::{extract_entry, staged_path};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::collections::HashSet;
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
    // we write anything. Catches the case where the user moved or deleted
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

    // A zip in the Black Hole stays present but is not read by Unarchive until it is restored.
    let mut black_holed: Vec<&str> = Vec::new();
    for zip in &unique_zip_paths {
        if crate::db::zip_is_in_black_hole(conn, zip)? {
            black_holed.push(zip);
        }
    }
    if !black_holed.is_empty() {
        black_holed.sort();
        let msg = black_holed
            .iter()
            .map(|z| crate::db::zip_in_black_hole_message(z))
            .collect::<Vec<_>>()
            .join("\n");
        tracing::warn!(operation_id, zips = black_holed.len(), "unarchive refused: zip is in the Black Hole");
        anyhow::bail!(msg);
    }

    let mode = classify_target(target_root_path, &files);

    run_restore_inner(conn, operation_id, &op, &files, total, mode,
                      overwrite_existing, keep_zip_after_restore, cancel, emitter)
}

#[allow(clippy::too_many_arguments)]
fn run_restore_inner(
    conn: &Connection,
    operation_id: i64,
    op: &crate::archive::models::ArchiveOperation,
    files: &[crate::archive::models::ArchiveOperationFile],
    total: usize,
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
    };

    // Stage: reconcile + extract -------------------------------------------------
    // For each file: skip if already on disk at source_path; else extract the
    // entry straight to the target (only entries whose target is missing are
    // read from their zip). We track which files actually got restored so the
    // catalog phase only touches those rows.
    let mut restored: Vec<(&crate::archive::models::ArchiveOperationFile, PathBuf)> =
        Vec::with_capacity(total);
    let mut conflicts: Vec<RestoreConflict> = Vec::new();
    let mut extracted = 0usize;

    for (idx, f) in files.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            anyhow::bail!("restore cancelled");
        }

        let original = Path::new(&f.source_path);
        // Pick the destination path based on mode.
        let dest = match mode {
            RestoreTargetMode::Original => original.to_path_buf(),
            RestoreTargetMode::UnderRoot(root) => {
                crate::archive::path_layout::dest_under_root(root, &f.target_path_in_zip)
            }
        };
        let mut already_in_place = original.is_file() && !overwrite_existing;
        // Restoring under another folder: a different file at the original path (a newer capture
        // that reused the name) is not in the way, because nothing is written there. When the
        // destination IS the original path the file there is checked like in Original mode.
        if already_in_place
            && dest != original
            && !matches!(compute_xxhash(original), Ok(ref h) if *h == f.expected_hash)
        {
            already_in_place = false;
        }

        // A file already at the place the frame belongs is trusted only by its hash: the original
        // path when the frame is "already in place", otherwise the final destination.
        let existing: Option<&Path> = if already_in_place {
            Some(original)
        } else if dest.exists() && !overwrite_existing {
            Some(dest.as_path())
        } else {
            None
        };

        // A trusted file already at the destination is adopted: the catalog update writes the row's
        // path, size and mtime from that verified file (the row may have been re-pointed at its zip's
        // virtual path, or the file may sit under another root), exactly as if it had just been copied.
        let mut adopt_existing = false;
        if let Some(existing) = existing {
            // Don't just trust whatever is sitting there — hash it with the SAME function that
            // produced the plan's expected_hash and compare. Anything else (wrong version,
            // different frame, half-written copy) is a conflict: leave it alone, don't clear
            // its markers, and keep going — see module docs.
            match compute_xxhash(existing) {
                Ok(actual) if actual == f.expected_hash => {
                    emit(
                        emitter,
                        "reconcile",
                        idx + 1,
                        total,
                        format!("Already on disk, hash-verified — skipping: {}", existing.display()),
                    );
                    adopt_existing = true;
                }
                Ok(actual) => {
                    let msg = format!(
                        "restore conflict: on-disk file does not match archived hash for {} (expected {}, got {})",
                        existing.display(), f.expected_hash, actual,
                    );
                    tracing::error!(
                        operation_id,
                        file_id = f.file_id.unwrap_or(-1),
                        src = %f.source_path,
                        dest = %existing.display(),
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
                        format!("CONFLICT — on-disk file doesn't match the archive, left untouched: {}", existing.display()),
                    );
                }
                Err(e) => {
                    // Unreadable is treated exactly like a mismatch: never
                    // panic, never fall through to skip-and-clear.
                    let msg = format!(
                        "restore conflict: could not hash on-disk file at {}: {:#}",
                        existing.display(), e,
                    );
                    tracing::error!(
                        operation_id,
                        file_id = f.file_id.unwrap_or(-1),
                        src = %f.source_path,
                        dest = %existing.display(),
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
                        format!("CONFLICT — could not verify on-disk file, left untouched: {}", existing.display()),
                    );
                }
            }
            if !adopt_existing {
                continue;
            }
        }

        // The path the catalog row will point at: the verified file itself when one is adopted (it may
        // sit at the original path while `dest` is under another root), otherwise `dest`.
        let record_path: PathBuf = match (adopt_existing, existing) {
            (true, Some(e)) => e.to_path_buf(),
            _ => dest.clone(),
        };

        // The catalog update writes `files.path = record_path`, which is UNIQUE: if another row already
        // owns it, copying would leave a file on disk and fail the catalog step. Check first.
        if let (Some(file_id), Some(dest_str)) = (f.file_id, record_path.to_str()) {
            let owner: Option<i64> = conn
                .query_row(
                    "SELECT id FROM files WHERE path = ?1 AND id != ?2",
                    rusqlite::params![dest_str, file_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(owner_id) = owner {
                let msg = format!(
                    "restore conflict: another catalog frame owns this path ({}, file {})",
                    record_path.display(), owner_id,
                );
                tracing::error!(
                    operation_id,
                    file_id,
                    owner_file_id = owner_id,
                    dest = %record_path.display(),
                    "restore conflict: another catalog frame owns the destination path, leaving frame set archived pending resolution"
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
                    format!("CONFLICT — another catalog frame owns this path, nothing copied: {}", record_path.display()),
                );
                continue;
            }
        }

        if adopt_existing {
            // Nothing to copy: the catalog update below points the row at the verified file.
            restored.push((f, record_path));
            continue;
        }

        // The target is free (or `overwrite_existing` asks for a replacement): extract the entry there.
        emit(
            emitter,
            "extract",
            idx + 1,
            total,
            format!("Extracting {}/{}", idx + 1, total),
        );
        // The plan's hash of an overwrite extraction's staged file, known before anything at the target is touched.
        let mut checked_hash: Option<Result<String>> = None;
        let mut staged_mismatch = false;
        let placed = if overwrite_existing {
            // The caller asked to replace whatever is at the target, but only with a verified copy: the
            // entry goes to a staging file beside it, is hashed on the way, and is renamed onto the
            // target only when it matches the plan. A mismatch leaves the file that is there untouched.
            // A run killed during an earlier overwrite left its staging leftovers here; place_entry sweeps
            // them on the other path.
            crate::zip_store::cache::sweep_stale_beside(&dest);
            let staged = staged_path(&dest);
            extract_entry(
                Path::new(&f.target_zip_path), &f.target_path_in_zip, None, &staged, cancel.as_ref(), &mut |_| {},
            )
            .with_context(|| format!("extract {} from {}", f.target_path_in_zip, f.target_zip_path))?;
            // `expected_hash` is `compute_xxhash` (three sampled blocks), not the full-file hash the
            // extraction returns, so the staged file is judged with the function that produced it.
            let staged_verdict = compute_xxhash(&staged);
            let staged_ok = matches!(&staged_verdict, Ok(h) if *h == f.expected_hash);
            if staged_ok {
                if let Err(e) = std::fs::rename(&staged, &dest) {
                    let _ = std::fs::remove_file(&staged);
                    return Err(e).with_context(|| format!("rename {} to {}", staged.display(), dest.display()));
                }
            } else {
                if let Err(e) = std::fs::remove_file(&staged) {
                    tracing::warn!(path = %staged.display(), error = %e, "could not remove the staging file that failed the integrity check");
                }
                staged_mismatch = true;
            }
            checked_hash = Some(staged_verdict);
            PlaceOutcome::Placed
        } else {
            place_entry(Path::new(&f.target_zip_path), &f.target_path_in_zip, None, &dest, cancel.as_ref())
                .with_context(|| format!("extract {} from {}", f.target_path_in_zip, f.target_zip_path))?
        };
        // `dest` holds a file this run wrote only when the placement happened and, for an overwrite, passed the check.
        let we_placed = placed == PlaceOutcome::Placed && !staged_mismatch;
        if let PlaceOutcome::Conflict(reason) = &placed {
            // A file appeared at the target after the check above and it is not the frame's own bytes.
            let msg = format!("restore conflict: {reason}");
            tracing::error!(
                operation_id,
                file_id = f.file_id.unwrap_or(-1),
                src = %f.source_path,
                dest = %dest.display(),
                "restore conflict: a different file took the target while it was being restored, leaving frame set archived pending resolution"
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
                format!("CONFLICT — a different file took the target, left untouched: {}", dest.display()),
            );
            continue;
        }

        // Verify the placed file against the plan with the function that produced expected_hash: catches
        // a corrupt zip. What this run placed is removed again; a file that was already there is not ours.
        let verdict = match checked_hash {
            Some(v) => v,
            None => compute_xxhash(&dest),
        };
        let ok = matches!(&verdict, Ok(h) if *h == f.expected_hash);
        if !ok {
            let actual = verdict.as_ref().ok().cloned();
            let msg = match &verdict {
                Ok(h) => format!(
                    "restore conflict: extracted file does not match archived hash for {} (expected {}, got {})",
                    f.target_path_in_zip, f.expected_hash, h,
                ),
                Err(e) => format!("restore conflict: could not hash {}: {:#}", dest.display(), e),
            };
            tracing::error!(
                operation_id,
                file_id = f.file_id.unwrap_or(-1),
                src = %f.source_path,
                dest = %dest.display(),
                expected_hash = %f.expected_hash,
                actual_hash = actual.as_deref().unwrap_or(""),
                "restore conflict: extracted file failed the integrity check, leaving frame set archived pending resolution"
            );
            if we_placed {
                if let Err(e) = std::fs::remove_file(&dest) {
                    tracing::warn!(dest = %dest.display(), error = %e, "could not remove the file that failed the integrity check");
                }
            }
            record_conflict_step(conn, operation_id, f, actual.as_deref(), &msg)?;
            conflicts.push(RestoreConflict {
                file_id: f.file_id,
                source_path: f.source_path.clone(),
                expected_hash: f.expected_hash.clone(),
                actual_hash: actual,
            });
            emit(
                emitter,
                "reconcile",
                idx + 1,
                total,
                format!("CONFLICT — extracted file failed the integrity check: {}", f.target_path_in_zip),
            );
            continue;
        }

        if we_placed {
            extracted += 1;
        }
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

            // Rewrite path only if it differs from the row's current path (normally the
            // source_path; a zip path when a different file took the original one and the scanner
            // moved this row to its virtual path); mtime/size are always synced (they're the whole
            // point of this fix).
            let current_path: Option<String> = conn
                .query_row("SELECT path FROM files WHERE id = ?1", rusqlite::params![file_id], |r| r.get(0))
                .optional()?;
            let path_changed = new_path.to_str() != Some(current_path.as_deref().unwrap_or(f.source_path.as_str()));
            let path_arg = if path_changed { new_path.to_str() } else { None };
            update_restored_row(conn, operation_id, f, Some((file_id, path_arg, &new_mtime, new_size)))?;
        } else {
            update_restored_row(conn, operation_id, f, None)?;
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
            // This operation's markers are cleared by now. Any row still pointing into the zip (a frame
            // added to the zip by hand and cataloged from it, or one adopted from it) lives only there.
            let remaining: i64 = conn.query_row(
                "SELECT COUNT(*) FROM files WHERE archive_zip_path = ?1",
                rusqlite::params![zp],
                |r| r.get(0),
            )?;
            if remaining > 0 {
                tracing::warn!(
                    operation_id,
                    zip_path = %zp,
                    count = remaining,
                    "keeping the zip after restore: catalog rows still live in it"
                );
                emit(
                    emitter,
                    "cleanup",
                    idx + 1,
                    cleanup_total,
                    format!("Keeping zip {}/{} — {} catalog frame(s) still live in it", idx + 1, cleanup_total, remaining),
                );
                continue;
            }
            match std::fs::remove_file(zp) {
                Ok(()) => {
                    // The zip is gone: its releases and its (possibly placeholder) row go with it, so a
                    // later zip of the same name starts clean.
                    forget_zip(conn, zp)?;
                }
                Err(e) => {
                    tracing::warn!(operation_id, zip_path = %zp, error = %e, "failed to remove zip after restore; file left in place");
                }
            }
            emit(
                emitter,
                "cleanup",
                idx + 1,
                cleanup_total,
                format!("Removing zip {}/{}", idx + 1, cleanup_total),
            );
        }
    }

    Ok(RestoreOutcome { conflicts, extracted })
}

/// The catalog side of one restored entry, in ONE transaction: the row becomes a disk row (when it has
/// one) and the entry is released from its zip. The entry now lives on disk, so a later scan of the zip
/// (kept for any reason) must not catalog it again; with the two writes apart, a crash between them would
/// leave a disk row whose entry is not released, and that scan would add the entry as a foreign duplicate.
/// Cleanup drops the releases again together with a zip it deletes.
fn update_restored_row(
    conn: &Connection,
    operation_id: i64,
    f: &ArchiveOperationFile,
    row: Option<(i64, Option<&str>, &str, i64)>,
) -> Result<()> {
    let sp = crate::db::SavepointGuard::new(conn, "restore_release_entry")?;
    if let Some((file_id, path_arg, new_mtime, new_size)) = row {
        adb::unmark_file_archived(conn, file_id, path_arg, Some(new_mtime), Some(new_size))?;
    }
    record_released_entry(conn, &f.target_zip_path, &f.target_path_in_zip, Some(operation_id), row.map(|r| r.0))
        .with_context(|| format!("record the released entry {} of {}", f.target_path_in_zip, f.target_zip_path))?;
    sp.commit()?;
    Ok(())
}

/// Drops the catalog's memory of a zip that was deleted: the entries released from it and its row.
pub(crate) fn forget_zip(conn: &Connection, zip: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM zip_released_entries WHERE zip_id IN (SELECT id FROM zip_archives WHERE path = ?1)",
        rusqlite::params![zip],
    )?;
    conn.execute("DELETE FROM zip_archives WHERE path = ?1", rusqlite::params![zip])?;
    Ok(())
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
        assert!(!arch.path().join(".athenaeum_restore_temp").exists(), "restore leaves no temp tree");
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

    /// Archive a one-frame set (`bytes` at `<scan>/M31/L_001.fits`, scan root id 1) whose plan also
    /// holds a frame of a second root, so the restore target can be classed `UnderRoot` while the
    /// destination still equals the original path. Returns (op id, original path of the first frame,
    /// its file id).
    fn archive_two_roots(conn: &Connection, arch: &Path, scan: &Path, other: &Path) -> (i64, PathBuf, i64) {
        let l1 = scan.join("M31/L_001.fits");
        let l2 = other.join("M31/L_002.fits");
        for (p, b) in [(&l1, b"first-frame".as_slice()), (&l2, b"second-frame".as_slice())] {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b).unwrap();
        }
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [scan.to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (2, ?1)", [other.to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        for (fid, p, name) in [(1000, &l1, "L_001.fits"), (1001, &l2, "L_002.fits")] {
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, 11, '2025-10-12', 'FITS')",
                params![fid, p.to_str().unwrap(), name],
            ).unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp) VALUES (?1, ?2, 'M31', 'T', 'C', 'Light')",
                params![fid * 10, fid],
            ).unwrap();
            conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)", [fid * 10]).unwrap();
        }
        let plan = build_plan(
            conn, 1, arch,
            &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
            ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(conn, &plan, ConflictResolution::Overwrite).unwrap();
        run_operation(conn, op_id, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
        assert!(!l1.exists() && !l2.exists());
        (op_id, l1, 1000)
    }

    fn zip_count(dir: &Path) -> usize {
        std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "zip")).count()
    }

    fn frame_set_archived(conn: &Connection) -> bool {
        let at: Option<String> = conn.query_row("SELECT archived_at FROM frames_set WHERE id = 1", [], |r| r.get(0)).unwrap();
        at.is_some()
    }

    /// Restoring "into the parent folder" of a scan root while the plan has a frame elsewhere is an
    /// alternate-root restore whose destination equals the original path. A newer capture there must
    /// not be replaced by "skipping": conflict, zip kept, set stays archived, the capture untouched.
    #[test]
    fn under_root_restore_onto_the_original_path_keeps_a_new_capture_as_a_conflict() {
        let base = TempDir::new().unwrap();
        let arch = TempDir::new().unwrap();
        let scan = base.path().join("x/rootA");
        let other = base.path().join("y/rootB");
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (op_id, original, _) = archive_two_roots(&conn, arch.path(), &scan, &other);
        std::fs::create_dir_all(original.parent().unwrap()).unwrap();
        std::fs::write(&original, b"a newer capture").unwrap();

                let files = adb::list_operation_files(&conn, op_id).unwrap();
        assert!(matches!(classify_target(&base.path().join("x"), &files), RestoreTargetMode::UnderRoot(_)), "this restore is an alternate-root one");
        let dest0 = crate::archive::path_layout::dest_under_root(&base.path().join("x"), &files.iter().find(|f| f.source_path == original.to_str().unwrap()).unwrap().target_path_in_zip);
        assert_eq!(dest0, original, "the destination equals the original path");

        let outcome = run_restore(&conn, op_id, &base.path().join("x"), false, false, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
        assert_eq!(outcome.conflicts.len(), 1, "{:?}", outcome.conflicts);
        assert_eq!(std::fs::read(&original).unwrap(), b"a newer capture");
        assert!(zip_count(arch.path()) > 0, "the zip is kept");
        assert!(frame_set_archived(&conn));
        let markers: i64 = conn.query_row("SELECT COUNT(*) FROM files WHERE archive_zip_path IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert!(markers > 0, "markers stay");
    }

    /// An unrelated file at the final destination of an alternate-root restore is a conflict, not a
    /// "Skipping (target exists)" followed by marker clearing and zip removal.
    #[test]
    fn under_root_restore_with_an_unrelated_file_at_the_destination_is_a_conflict() {
        let base = TempDir::new().unwrap();
        let arch = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        let scan = base.path().join("x/rootA");
        let other = base.path().join("y/rootB");
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (op_id, _, file_id) = archive_two_roots(&conn, arch.path(), &scan, &other);
        let dest = target.path().join("rootA/M31/L_001.fits");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"unrelated").unwrap();

        let outcome = run_restore(&conn, op_id, target.path(), false, false, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
        assert_eq!(outcome.conflicts.len(), 1, "{:?}", outcome.conflicts);
        assert_eq!(outcome.conflicts[0].file_id, Some(file_id));
        assert_eq!(std::fs::read(&dest).unwrap(), b"unrelated");
        assert!(zip_count(arch.path()) > 0, "the zip is kept");
        assert!(frame_set_archived(&conn));
        let zip_col: Option<String> = conn.query_row("SELECT archive_zip_path FROM files WHERE id = ?1", [file_id], |r| r.get(0)).unwrap();
        assert!(zip_col.is_some(), "markers stay");
    }

    /// A file at the destination that IS the frame (same hash) is still trusted: no conflict.
    #[test]
    fn under_root_restore_with_the_frames_own_bytes_at_the_destination_is_not_a_conflict() {
        let base = TempDir::new().unwrap();
        let arch = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        let scan = base.path().join("x/rootA");
        let other = base.path().join("y/rootB");
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (op_id, _, _) = archive_two_roots(&conn, arch.path(), &scan, &other);
        let dest = target.path().join("rootA/M31/L_001.fits");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"first-frame").unwrap();

        let outcome = run_restore(&conn, op_id, target.path(), false, false, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
        assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
        assert!(!frame_set_archived(&conn));
        // The trusted file is adopted: the row now lives at the destination, markers cleared.
        let (path, zip, op_col): (String, Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT path, archive_zip_path, archived_in_operation FROM files WHERE id = 1000",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(path, dest.to_str().unwrap(), "files.path follows the adopted file");
        assert!(zip.is_none() && op_col.is_none(), "markers cleared");
    }

    /// Restoring under another root while the frame's own bytes already sit at its original path (put
    /// back by hand): that verified file is adopted where it is. The restore succeeds, `files.path` is
    /// the original path, markers are cleared, and nothing is copied to the destination.
    #[test]
    fn under_root_restore_adopts_the_frames_own_bytes_at_the_original_path() {
        let base = TempDir::new().unwrap();
        let arch = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        let scan = base.path().join("x/rootA");
        let other = base.path().join("y/rootB");
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (op_id, original, file_id) = archive_two_roots(&conn, arch.path(), &scan, &other);
        std::fs::create_dir_all(original.parent().unwrap()).unwrap();
        std::fs::write(&original, b"first-frame").unwrap();
        let dest = target.path().join("rootA/M31/L_001.fits");
        assert_ne!(dest, original);

        let outcome = run_restore(&conn, op_id, target.path(), false, false, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
        assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
        assert!(!dest.exists(), "nothing is copied next to a verified file");
        assert_eq!(std::fs::read(&original).unwrap(), b"first-frame");
        let (path, zip, op_col): (String, Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT path, archive_zip_path, archived_in_operation FROM files WHERE id = ?1",
                [file_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(path, original.to_str().unwrap(), "files.path is the verified file's path");
        assert!(zip.is_none() && op_col.is_none(), "markers cleared");
    }

    /// A row the scanner re-pointed at its zip's virtual path, whose verified bytes sit at the
    /// original path: the restore moves `files.path` back to the original and takes size and mtime
    /// from that file.
    #[test]
    fn trusted_file_at_the_original_path_repoints_a_row_that_reads_as_zip_resident() {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();
        let l1 = scan.path().join("M31/L_001.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        std::fs::write(&l1, b"first-frame").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (1000, ?1, 'L_001.fits', 11, '2025-10-12', 'FITS')",
            [l1.to_str().unwrap()],
        ).unwrap();
        conn.execute("INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp) VALUES (10000, 1000, 'M31', 'T', 'C', 'Light')", []).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000)", []).unwrap();
        let plan = build_plan(
            &conn, 1, arch.path(),
            &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
            ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
        run_operation(&conn, op_id, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
        assert!(!l1.exists());

        // The scanner re-pointed the row at its virtual path; the same bytes are back at the original path.
        let (zip, entry): (String, String) = conn
            .query_row("SELECT archive_zip_path, archive_path_in_zip FROM files WHERE id = 1000", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        conn.execute(
            "UPDATE files SET path = ?1, size = 99, modified_at = '2000-01-01T00:00:00+00:00' WHERE id = 1000",
            [crate::zip_store::scan::virtual_path(Path::new(&zip), &entry)],
        ).unwrap();
        std::fs::write(&l1, b"first-frame").unwrap();

        let outcome = run_restore(&conn, op_id, scan.path(), false, false, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
        assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
        let (path, size, mtime, zip_col): (String, i64, String, Option<String>) = conn
            .query_row(
                "SELECT path, size, modified_at, archive_zip_path FROM files WHERE id = 1000",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(path, l1.to_str().unwrap(), "files.path is the verified file, not the virtual path");
        assert_eq!(size, 11, "size comes from the file on disk");
        assert_ne!(mtime, "2000-01-01T00:00:00+00:00", "mtime comes from the file on disk");
        assert!(zip_col.is_none(), "markers cleared");
        assert_eq!(std::fs::read(&l1).unwrap(), b"first-frame");
    }

    /// The new capture's row still owns the original path (its file was deleted from disk): the
    /// restore reports a conflict BEFORE copying, so no file is left behind and the zip stays.
    #[test]
    fn restore_onto_a_path_owned_by_another_catalog_row_copies_nothing() {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();
        let l1 = scan.path().join("M31/L_001.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        std::fs::write(&l1, b"first-frame").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (1000, ?1, 'L_001.fits', 11, '2025-10-12', 'FITS')",
            [l1.to_str().unwrap()],
        ).unwrap();
        conn.execute("INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp) VALUES (10000, 1000, 'M31', 'T', 'C', 'Light')", []).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000)", []).unwrap();
        let plan = build_plan(
            &conn, 1, arch.path(),
            &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
            ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
        run_operation(&conn, op_id, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
        assert!(!l1.exists());

        // The archived row was re-pointed at its zip; a newer capture's row took the original path,
        // and its file is gone from disk.
        let (zip, entry): (String, String) = conn
            .query_row("SELECT archive_zip_path, archive_path_in_zip FROM files WHERE id = 1000", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        conn.execute("UPDATE files SET path = ?1 WHERE id = 1000", [crate::zip_store::scan::virtual_path(Path::new(&zip), &entry)]).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (2000, ?1, 'L_001.fits', 5, '2025-10-13', 'FITS')",
            [l1.to_str().unwrap()],
        ).unwrap();

        let outcome = run_restore(&conn, op_id, scan.path(), false, false, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
        assert_eq!(outcome.conflicts.len(), 1, "{:?}", outcome.conflicts);
        assert!(!l1.exists(), "nothing was copied");
        assert!(zip_count(arch.path()) > 0, "the zip is kept");
        assert!(frame_set_archived(&conn));
        let p: String = conn.query_row("SELECT path FROM files WHERE id = 2000", [], |r| r.get(0)).unwrap();
        assert_eq!(p, l1.to_str().unwrap());
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

        // Nothing is written before the preflight bail.
        assert!(!arch.path().join(".athenaeum_restore_temp").exists());
        assert!(std::fs::read_dir(restore_target.path()).unwrap().next().is_none(), "nothing extracted");
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
            cancel2, false,
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

    // ---- extraction straight to the target (no temp tree) -------------------------------------

    /// A frame set of `n` lights archived (moved) into zips under `arch`; returns the originals' paths.
    fn archived_lights(n: usize) -> (TempDir, TempDir, Connection, i64, Vec<PathBuf>) {
        archived_lights_with(n, |i| format!("light-content-{i}").into_bytes())
    }

    /// Frames of `size` bytes whose content varies along the file, so that the sampled hash and the
    /// full-file hash differ (they agree only for files of 512 KiB or less).
    fn big_content(i: usize, size: usize) -> Vec<u8> {
        (0..size).map(|k| (k.wrapping_mul(31).wrapping_add(k >> 9).wrapping_add(i * 7) % 251) as u8).collect()
    }

    fn archived_lights_with(
        n: usize,
        content: impl Fn(usize) -> Vec<u8>,
    ) -> (TempDir, TempDir, Connection, i64, Vec<PathBuf>) {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (10, 1, '2025-10-12', '2025-10-13')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        let mut paths = Vec::new();
        for i in 0..n {
            let p = scan.path().join(format!("M31/L_{i:03}.fits"));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, content(i)).unwrap();
            let (fid, frid) = (1000 + i as i64, 10000 + i as i64);
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, 16, '2025-10-12', 'FITS')",
                params![fid, p.to_str().unwrap(), format!("L_{i:03}.fits")],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp) VALUES (?1, ?2, 'M31', 'T', 'C', 'Light')",
                params![frid, fid],
            )
            .unwrap();
            conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)", [frid]).unwrap();
            paths.push(p);
        }
        let plan = build_plan(
            &conn,
            1,
            arch.path(),
            &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
            ArchiveCompression::Store,
        )
        .unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
        run_operation(&conn, op_id, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
        for p in &paths {
            assert!(!p.exists(), "the archive moved {}", p.display());
        }
        (arch, scan, conn, op_id, paths)
    }

    fn restore(conn: &Connection, op_id: i64, root: &Path, overwrite: bool, keep_zip: bool) -> RestoreOutcome {
        run_restore(conn, op_id, root, overwrite, keep_zip, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap()
    }

    #[test]
    fn unarchive_refuses_a_zip_in_the_black_hole_until_it_is_restored() {
        let (_arch, scan, conn, op_id, paths) = archived_lights(2);
        let zip: String = conn
            .query_row("SELECT target_zip_path FROM archive_operation_files WHERE operation_id = ?1 LIMIT 1", [op_id], |r| r.get(0))
            .unwrap();
        let bh = crate::db::add_zip_to_black_hole(&conn, &zip, "files").unwrap();

        let err = run_restore(&conn, op_id, scan.path(), false, true, &Arc::new(AtomicBool::new(false)), &NullEmitter)
            .unwrap_err()
            .to_string();
        assert!(err.contains("is in the Black Hole \u{2014} restore it first"), "{err}");
        assert!(err.contains(Path::new(&zip).file_name().unwrap().to_str().unwrap()), "{err}");
        for p in &paths {
            assert!(!p.exists(), "nothing was extracted");
        }

        crate::db::restore_zip_from_black_hole(&conn, bh).unwrap();
        let out = restore(&conn, op_id, scan.path(), false, true);
        assert_eq!(out.extracted, 2);
    }

    #[test]
    fn restore_extracts_only_the_missing_entries_and_records_released_ones_for_a_kept_zip() {
        let (arch, scan, conn, op_id, paths) = archived_lights(3);
        conn.execute("INSERT INTO fits_header (file_id, header, header_fingerprint) SELECT id, '', 'fp-' || id FROM files", []).unwrap();
        let first = restore(&conn, op_id, scan.path(), false, true);
        assert_eq!(first.extracted, 3);
        assert!(!arch.path().join(".athenaeum_restore_temp").exists(), "no temp tree is made");
        assert_eq!(zip_count(arch.path()), 1, "the zip is kept");
        let released: i64 = conn
            .query_row("SELECT COUNT(*) FROM zip_released_entries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(released, 3, "each restored entry is recorded for the kept zip");
        let linked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM zip_released_entries r INNER JOIN files f ON f.id = r.file_id WHERE f.archive_zip_path IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(linked, 3, "a restore links each release to the row it restored");
        let rows: Vec<(String, i64)> = conn
            .prepare("SELECT r.entry_path, r.file_id FROM zip_released_entries r ORDER BY r.id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for (entry, file_id) in rows {
            let target: String = conn
                .query_row("SELECT target_path_in_zip FROM archive_operation_files WHERE file_id = ?1", [file_id], |r| r.get(0))
                .unwrap();
            assert_eq!(entry, target, "each release points at the row of its own entry");
        }

        // Two files are deleted again; the next restore extracts exactly those two.
        std::fs::remove_file(&paths[0]).unwrap();
        std::fs::remove_file(&paths[2]).unwrap();
        let untouched_mtime = std::fs::metadata(&paths[1]).unwrap().modified().unwrap();
        let second = restore(&conn, op_id, scan.path(), false, true);
        assert_eq!(second.extracted, 2);
        assert!(!second.has_conflicts());
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"light-content-0");
        assert_eq!(std::fs::read(&paths[2]).unwrap(), b"light-content-2");
        assert_eq!(std::fs::metadata(&paths[1]).unwrap().modified().unwrap(), untouched_mtime);
        assert_eq!(
            conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM zip_released_entries", [], |r| r.get(0)).unwrap(),
            3,
            "recording is idempotent"
        );
    }

    #[test]
    fn unarchive_keeps_a_zip_that_a_catalog_row_still_lives_in() {
        let (arch, scan, conn, op_id, paths) = archived_lights(2);
        let zip: String = conn
            .query_row("SELECT archive_zip_path FROM files WHERE id = 1000", [], |r| r.get(0))
            .unwrap();
        // A frame added to the zip by hand and cataloged from it (a foreign row of the same zip).
        let vpath = format!("{zip}{}Hand{}X.fits", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR);
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, archive_zip_path, archive_path_in_zip)
             VALUES (7000, ?1, 'X.fits', 5, '2025-10-12', 'FITS', ?2, 'Hand/X.fits')",
            params![vpath, zip],
        )
        .unwrap();

        let outcome = restore(&conn, op_id, scan.path(), false, false);
        assert!(!outcome.has_conflicts());
        for p in &paths {
            assert!(p.exists());
        }
        assert!(Path::new(&zip).is_file(), "the zip with a live catalog row is not deleted");
        assert_eq!(zip_count(arch.path()), 1);
        let still: i64 = conn
            .query_row("SELECT COUNT(*) FROM files WHERE archive_zip_path = ?1", [&zip], |r| r.get(0))
            .unwrap();
        assert_eq!(still, 1);
        assert!(!frame_set_archived(&conn), "the frame set itself is restored");
        let released: i64 = conn.query_row("SELECT COUNT(*) FROM zip_released_entries", [], |r| r.get(0)).unwrap();
        assert_eq!(released, 2, "the restored entries are released in the zip that survives");
    }

    #[test]
    fn a_zip_kept_for_a_conflict_releases_the_entries_that_were_restored() {
        let (arch, scan, conn, op_id, paths) = archived_lights(2);
        conn.execute(
            "UPDATE archive_operation_files SET expected_hash = '0000000000000000' WHERE source_path = ?1",
            [paths[0].to_str().unwrap()],
        )
        .unwrap();
        let outcome = restore(&conn, op_id, scan.path(), false, false);
        assert_eq!(outcome.conflicts.len(), 1);
        assert_eq!(zip_count(arch.path()), 1, "a conflict keeps the zip");
        let released: Vec<String> = conn
            .prepare("SELECT entry_path FROM zip_released_entries")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(released.len(), 1, "only the restored entry is released: {released:?}");

        // A rescan of the kept zip must not catalog the restored frame a second time.
        let zip: String = conn
            .query_row("SELECT archive_zip_path FROM files WHERE id = 1000", [], |r| r.get(0))
            .unwrap();
        let before: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        crate::zip_store::scan::scan_zip(&conn, Path::new(&zip), 1, true).unwrap();
        let after: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(after, before, "no duplicate rows for the restored entries");
    }

    #[test]
    fn an_extracted_file_that_fails_the_integrity_check_is_removed_and_reported() {
        let (arch, scan, conn, op_id, paths) = archived_lights(2);
        conn.execute(
            "UPDATE archive_operation_files SET expected_hash = '0000000000000000' WHERE source_path = ?1",
            [paths[0].to_str().unwrap()],
        )
        .unwrap();
        let outcome = restore(&conn, op_id, scan.path(), false, false);
        assert_eq!(outcome.conflicts.len(), 1);
        assert_eq!(outcome.extracted, 1);
        assert!(!paths[0].exists(), "what this run placed is taken back");
        assert!(paths[1].exists());
        assert_eq!(zip_count(arch.path()), 1, "a conflict keeps the zip");
        assert!(frame_set_archived(&conn), "the frame set stays archived");
    }

    #[test]
    fn restore_with_overwrite_replaces_a_different_file_at_the_target() {
        let (_arch, scan, conn, op_id, paths) = archived_lights(1);
        std::fs::create_dir_all(paths[0].parent().unwrap()).unwrap();
        std::fs::write(&paths[0], b"a newer capture").unwrap();
        let outcome = restore(&conn, op_id, scan.path(), true, false);
        assert!(!outcome.has_conflicts());
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"light-content-0");
    }

    #[test]
    fn restore_never_replaces_a_different_file_without_overwrite() {
        let (arch, scan, conn, op_id, paths) = archived_lights(1);
        std::fs::create_dir_all(paths[0].parent().unwrap()).unwrap();
        std::fs::write(&paths[0], b"a newer capture").unwrap();
        let outcome = restore(&conn, op_id, scan.path(), false, false);
        assert_eq!(outcome.conflicts.len(), 1);
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"a newer capture");
        assert_eq!(zip_count(arch.path()), 1);
    }

    #[test]
    fn overwrite_with_a_bad_expected_hash_leaves_the_existing_file_untouched() {
        let (arch, scan, conn, op_id, paths) = archived_lights(1);
        std::fs::create_dir_all(paths[0].parent().unwrap()).unwrap();
        std::fs::write(&paths[0], b"a newer capture").unwrap();
        conn.execute("UPDATE archive_operation_files SET expected_hash = '0000000000000000'", []).unwrap();
        let outcome = restore(&conn, op_id, scan.path(), true, false);
        assert_eq!(outcome.conflicts.len(), 1);
        assert_eq!(outcome.extracted, 0);
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"a newer capture", "the file in the way is byte-for-byte");
        assert_eq!(zip_count(arch.path()), 1, "the zip is kept");
        assert!(frame_set_archived(&conn));
        let beside: Vec<String> = std::fs::read_dir(paths[0].parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(beside, vec!["L_000.fits"], "no staging file is left: {beside:?}");
    }

    const BIG: usize = 3 * 512 * 1024 + 4099;

    #[test]
    fn overwrite_restore_of_a_large_frame_passes_the_plans_sampled_hash() {
        // The full-file hash of this frame differs from `compute_xxhash`, which is what the plan stored.
        let (_arch, scan, conn, op_id, paths) = archived_lights_with(1, |i| big_content(i, BIG));
        let original = big_content(0, BIG);
        std::fs::create_dir_all(paths[0].parent().unwrap()).unwrap();
        std::fs::write(&paths[0], b"a newer capture").unwrap();
        let outcome = restore(&conn, op_id, scan.path(), true, false);
        assert!(!outcome.has_conflicts(), "{:?}", outcome.conflicts);
        assert_eq!(outcome.extracted, 1);
        assert!(std::fs::read(&paths[0]).unwrap() == original, "the entry replaced the file in the way");
        assert!(!frame_set_archived(&conn));
    }

    #[test]
    fn overwrite_with_a_bad_hash_on_a_large_frame_leaves_the_existing_file_untouched() {
        let (arch, scan, conn, op_id, paths) = archived_lights_with(1, |i| big_content(i, BIG));
        std::fs::create_dir_all(paths[0].parent().unwrap()).unwrap();
        std::fs::write(&paths[0], b"a newer capture").unwrap();
        conn.execute("UPDATE archive_operation_files SET expected_hash = '0000000000000000'", []).unwrap();
        let outcome = restore(&conn, op_id, scan.path(), true, false);
        assert_eq!(outcome.conflicts.len(), 1);
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"a newer capture");
        assert_eq!(zip_count(arch.path()), 1);
    }

    #[test]
    fn restore_of_a_large_frame_without_overwrite_succeeds() {
        let (_arch, scan, conn, op_id, paths) = archived_lights_with(1, |i| big_content(i, BIG));
        let outcome = restore(&conn, op_id, scan.path(), false, false);
        assert!(!outcome.has_conflicts(), "{:?}", outcome.conflicts);
        assert!(std::fs::read(&paths[0]).unwrap() == big_content(0, BIG));
    }

    #[test]
    fn a_clean_rerun_that_deletes_a_kept_zip_forgets_its_releases_and_row() {
        let (arch, scan, conn, op_id, paths) = archived_lights(2);
        let zip: String = conn
            .query_row("SELECT archive_zip_path FROM files WHERE id = 1000", [], |r| r.get(0))
            .unwrap();
        let good_hash: String = conn
            .query_row(
                "SELECT expected_hash FROM archive_operation_files WHERE source_path = ?1",
                [paths[0].to_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE archive_operation_files SET expected_hash = '0000000000000000' WHERE source_path = ?1",
            [paths[0].to_str().unwrap()],
        )
        .unwrap();
        let first = restore(&conn, op_id, scan.path(), false, false);
        assert_eq!(first.conflicts.len(), 1);
        assert_eq!(zip_count(arch.path()), 1);
        let zip_rows = || -> i64 {
            conn.query_row("SELECT COUNT(*) FROM zip_archives WHERE path = ?1", rusqlite::params![zip], |r| r.get(0)).unwrap()
        };
        let released_rows =
            || -> i64 { conn.query_row("SELECT COUNT(*) FROM zip_released_entries", [], |r| r.get(0)).unwrap() };
        assert_eq!(zip_rows(), 1, "the kept zip has its row");
        assert_eq!(released_rows(), 1, "and the entry that was restored");

        // The conflict is resolved (here: the plan's hash is right again) and the restore is re-run.
        conn.execute(
            "UPDATE archive_operation_files SET expected_hash = ?1 WHERE source_path = ?2",
            params![good_hash, paths[0].to_str().unwrap()],
        )
        .unwrap();
        let second = restore(&conn, op_id, scan.path(), false, false);
        assert!(!second.has_conflicts());
        assert_eq!(zip_count(arch.path()), 0, "the clean run deletes the zip");
        assert_eq!(zip_rows(), 0, "no row for a zip that is gone");
        assert_eq!(released_rows(), 0, "no releases for a zip that is gone");
    }

    /// The row's unmark and the entry's release are one transaction (spec 7.6): a failure of the second
    /// leaves the row exactly as it was, still marked with its zip, instead of a disk row whose entry a
    /// later scan of the kept zip would add again.
    #[test]
    fn the_unmark_and_the_release_of_a_restored_row_commit_together_or_not_at_all() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("PRAGMA foreign_keys = OFF", []).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, archived_in_operation, archive_zip_path, archive_path_in_zip, archive_entry_crc32)
             VALUES (7, '/orig/L.fits', 'L.fits', 1, '2025-01-01T00:00:00Z', 'FITS', 3, '/a/z.zip', 'L.fits', 99)",
            [],
        )
        .unwrap();
        let mut f = op_file("/orig/L.fits");
        f.target_zip_path = "/a/z.zip".into();
        f.target_path_in_zip = "L.fits".into();
        conn.execute_batch(
            "CREATE TRIGGER no_release BEFORE INSERT ON zip_released_entries BEGIN SELECT RAISE(ABORT, 'injected'); END",
        )
        .unwrap();

        let err = update_restored_row(&conn, 3, &f, Some((7, None, "2025-02-02T00:00:00Z", 5))).unwrap_err();
        assert!(format!("{err:#}").contains("injected"), "{err:#}");
        let (op, zip, size): (Option<i64>, Option<String>, i64) = conn
            .query_row("SELECT archived_in_operation, archive_zip_path, size FROM files WHERE id = 7", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!((op, zip.as_deref(), size), (Some(3), Some("/a/z.zip"), 1), "the unmark was undone");
        assert!(conn.is_autocommit(), "no transaction is left open");

        conn.execute_batch("DROP TRIGGER no_release").unwrap();
        update_restored_row(&conn, 3, &f, Some((7, None, "2025-02-02T00:00:00Z", 5))).unwrap();
        let (op, zip, size): (Option<i64>, Option<String>, i64) = conn
            .query_row("SELECT archived_in_operation, archive_zip_path, size FROM files WHERE id = 7", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!((op, zip, size), (None, None, 5));
        let released: i64 = conn.query_row("SELECT COUNT(*) FROM zip_released_entries", [], |r| r.get(0)).unwrap();
        assert_eq!(released, 1);
    }
}
