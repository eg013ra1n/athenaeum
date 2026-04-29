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
//!                   restore (copy temp → target). Target is either the
//!                   original source_path (preserves layout) or
//!                   `<picked>/<path-in-zip>` for an alternate target.
//! - update_catalog: clear archive markers for restored files; rewrite
//!                   `files.path` only when the restore target differs from
//!                   the original.
//! - cleanup:        delete temp dir, optionally delete zips.

use crate::archive::db as adb;
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
    let target = target_root_path.to_string_lossy();
    let all_under = files.iter().all(|f| f.source_path.starts_with(target.as_ref()));
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
) -> Result<()> {
    let op = adb::get_operation(conn, operation_id)?;
    let files = adb::list_operation_files(conn, operation_id)?;
    let total = files.len();
    if total == 0 {
        return Ok(());
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
        Ok(()) => Ok(()),
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
) -> Result<()> {

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

    for (idx, (f, temp_path)) in files.iter().zip(temp_paths.iter()).enumerate() {
        if cancel.load(Ordering::SeqCst) {
            cleanup_temp(&temp_dir);
            anyhow::bail!("restore cancelled");
        }

        let original = Path::new(&f.source_path);
        let already_in_place = original.is_file() && !overwrite_existing;

        if already_in_place {
            emit(
                emitter,
                "reconcile",
                idx + 1,
                total,
                format!("Skipping (already on disk): {}", f.source_path),
            );
            continue;
        }

        // Pick the destination path based on mode.
        let dest = match mode {
            RestoreTargetMode::Original => original.to_path_buf(),
            RestoreTargetMode::UnderRoot(root) => root.join(&f.target_path_in_zip),
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
    // For each restored file, clear archive markers and rewrite path if it
    // changed. Files that were skipped (already on disk) need no catalog
    // change — their rows were either never marked (copy disposition) or
    // their `path` already matched the on-disk location.
    let catalog_total = restored.len() + 1;
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
            // If the new path differs from the catalog's source_path, rewrite it.
            // Otherwise just clear the archive markers in place.
            let path_changed = new_path.to_str() != Some(f.source_path.as_str());
            if path_changed {
                adb::unmark_file_archived(conn, file_id, Some(new_path.to_str().unwrap()))?;
            } else {
                adb::unmark_file_archived(conn, file_id, None)?;
            }
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
    // frame set's archived_at must still be cleared so it returns to active view.
    adb::unmark_frame_set_archived(conn, op.frames_set_id)?;
    catalog_done += 1;
    emit(
        emitter,
        "update_catalog",
        catalog_done,
        catalog_total,
        "Frame set unarchived".into(),
    );

    // For files that weren't restored (skipped because already on disk), still
    // clear any leftover archive markers on their catalog rows. Otherwise the
    // row would keep pointing to a now-orphaned zip path.
    let restored_ids: HashSet<i64> = restored
        .iter()
        .filter_map(|(f, _)| f.file_id)
        .collect();
    for f in files {
        if let Some(fid) = f.file_id {
            if !restored_ids.contains(&fid) {
                adb::unmark_file_archived(conn, fid, None)?;
            }
        }
    }

    // Stage: cleanup -----------------------------------------------------------
    cleanup_temp(temp_dir);

    if !keep_zip_after_restore {
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

    Ok(())
}

fn cleanup_temp(temp_dir: &Path) {
    if temp_dir.exists() {
        let _ = std::fs::remove_dir_all(temp_dir);
    }
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
    use tempfile::TempDir;

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
        run_restore(
            &conn, op_id, scan.path(),
            false, false, &cancel, &NullEmitter,
        ).unwrap();

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
}
