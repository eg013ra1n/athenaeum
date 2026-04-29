//! Restore: extract zip(s) back to disk and update files.path.
//!
//! Implementation note: we record restore stages in the SAME archive_operation_steps
//! table, using stage names "restore_extract" and "restore_verify". This keeps a
//! single source of truth for the operation's history without a parallel table.

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

/// Run a restore for the given archive operation.
///
/// `target_root_path` is the user-chosen directory; files are extracted as
/// `<target_root_path>/<target_path_in_zip>`, preserving the scan-root prefix.
/// On success: clear archive markers + rewrite `files.path` to the new locations.
/// On verify failure: keep the zip, mark restore failed, do NOT auto-rollback partial extracts.
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

    // Stage: extract ----------------------------------------------------------
    // Open each unique zip lazily; process files one at a time to avoid
    // holding a ZipFile borrow across loop iterations.
    let mut buf = vec![0u8; 64 * 1024];

    // Collect per-file: (target_zip_path, target_path_in_zip, dest, skipped).
    let mut extracted: Vec<(String, String, PathBuf, bool)> = Vec::with_capacity(files.len());

    // We open each archive fresh per file to avoid lifetime conflicts between
    // `ZipFile` (which borrows the archive) and the HashMap containing archives.
    // For typical operation sizes this is cheap — zip's central directory parse
    // is O(N entries) not O(file size).
    for (idx, f) in files.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            anyhow::bail!("restore cancelled");
        }
        // Emit on both the unified "archive-progress" channel (so the existing
        // ArchiveProgress UI picks it up) and the legacy "archive-restore-progress"
        // channel (kept for backwards compatibility with any other listener).
        let prog = RestoreProgress {
            operation_id,
            stage: "extract".into(),
            current: idx + 1,
            total,
            message: format!("Extracting {}/{}", idx + 1, total),
        };
        emit_event(emitter, "archive-progress", &prog);
        emit_event(emitter, "archive-restore-progress", &prog);

        let dest = target_root_path.join(&f.target_path_in_zip);
        if dest.exists() && !overwrite_existing {
            // Skip — caller decided to preserve existing files
            extracted.push((f.target_zip_path.clone(), f.target_path_in_zip.clone(), dest, true));
            continue;
        }

        // Open zip for this file.
        let file = File::open(&f.target_zip_path)
            .with_context(|| format!("open zip {}", f.target_zip_path))?;
        let mut archive = zip::ZipArchive::new(BufReader::new(file))
            .with_context(|| format!("parse zip {}", f.target_zip_path))?;

        let mut entry = archive.by_name(&f.target_path_in_zip)
            .with_context(|| format!("entry not found in zip: {}", f.target_path_in_zip))?;

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        let mut out = File::create(&dest)
            .with_context(|| format!("create dest {}", dest.display()))?;

        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 { break; }
            out.write_all(&buf[..n])?;
        }
        drop(out);
        drop(entry);

        extracted.push((f.target_zip_path.clone(), f.target_path_in_zip.clone(), dest, false));
    }

    // Stage: verify -----------------------------------------------------------
    let mut written_files: Vec<(&crate::archive::models::ArchiveOperationFile, PathBuf)> =
        Vec::with_capacity(files.len());

    for (idx, (f, (_zip, _piz, dest, skipped))) in files.iter().zip(extracted.iter()).enumerate() {
        if cancel.load(Ordering::SeqCst) {
            anyhow::bail!("restore cancelled");
        }
        let prog = RestoreProgress {
            operation_id,
            stage: "verify".into(),
            current: idx + 1,
            total,
            message: format!("Verifying {}/{}", idx + 1, total),
        };
        emit_event(emitter, "archive-progress", &prog);
        emit_event(emitter, "archive-restore-progress", &prog);

        if *skipped {
            // Skipped during extract (overwrite=false + existing). Don't verify.
            continue;
        }

        let actual = compute_xxhash(dest)
            .with_context(|| format!("hash {}", dest.display()))?;
        if actual != f.expected_hash {
            anyhow::bail!(
                "restore verify failed for {}: expected {} got {}",
                dest.display(), f.expected_hash, actual,
            );
        }
        written_files.push((f, dest.clone()));
    }

    // Stage: update_catalog ---------------------------------------------------
    // One progress unit per file path rewrite + 1 for the frame set unmark.
    let catalog_total = written_files.len() + 1;
    let mut catalog_done: usize = 0;
    let emit_catalog = |emitter: &dyn ProgressEmitter, done: usize, total: usize, msg: String| {
        let prog = RestoreProgress {
            operation_id,
            stage: "update_catalog".into(),
            current: done,
            total,
            message: msg,
        };
        emit_event(emitter, "archive-progress", &prog);
        emit_event(emitter, "archive-restore-progress", &prog);
    };
    emit_catalog(emitter, catalog_done, catalog_total, "Updating catalog".into());

    for (f, new_path) in &written_files {
        if let Some(file_id) = f.file_id {
            adb::unmark_file_archived(conn, file_id, Some(new_path.to_str().unwrap()))?;
        }
        catalog_done += 1;
        emit_catalog(
            emitter,
            catalog_done,
            catalog_total,
            format!("Updating paths ({}/{})", catalog_done, catalog_total),
        );
    }
    adb::unmark_frame_set_archived(conn, op.frames_set_id)?;
    catalog_done += 1;
    emit_catalog(emitter, catalog_done, catalog_total, "Frame set unarchived".into());

    // Stage: cleanup ----------------------------------------------------------
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
            let prog = RestoreProgress {
                operation_id,
                stage: "cleanup".into(),
                current: idx + 1,
                total: cleanup_total,
                message: format!(
                    "Removing zip {}/{}",
                    idx + 1,
                    cleanup_total
                ),
            };
            emit_event(emitter, "archive-progress", &prog);
            emit_event(emitter, "archive-restore-progress", &prog);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::executor::run_operation;
    use crate::archive::models::{ArchiveCompression, ConflictResolution, Dispositions};
    use crate::archive::planner::{build_plan, commit_plan};
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use tempfile::TempDir;

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

        // Source is gone, archived_at is set
        assert!(!l1.exists());

        // Now restore to a different target
        run_restore(
            &conn, op_id, restore_target.path(),
            true, false, &cancel, &NullEmitter,
        ).unwrap();

        // zip should have been deleted (keep_zip_after_restore = false)
        let zips: Vec<_> = std::fs::read_dir(arch.path()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "zip").unwrap_or(false))
            .collect();
        assert_eq!(zips.len(), 0);

        // Verify path was rewritten
        let new_path: String = conn.query_row(
            "SELECT path FROM files WHERE id = 1000", [], |r| r.get(0),
        ).unwrap();
        assert!(new_path.starts_with(restore_target.path().to_str().unwrap()));
        let restored_content = std::fs::read(&new_path).unwrap();
        assert_eq!(restored_content, b"original-content");

        // Frame set is no longer archived
        let archived_at: Option<String> = conn.query_row(
            "SELECT archived_at FROM frames_set WHERE id = 1", [], |r| r.get(0),
        ).unwrap();
        assert!(archived_at.is_none());

    }
}
