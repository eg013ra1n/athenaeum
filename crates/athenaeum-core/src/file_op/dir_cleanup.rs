//! After a Move completes, remove the folders it emptied at the source.
//!
//! The planner expands a selected directory into one row per file, with
//! `dest = dest_dir/<directory name>/<relative path>`, so the rows alone say
//! which directories were moved: a row whose destination sits more than one
//! level under `file_operations.dest_dir` came from the directory named by
//! that first component, and the directory itself is the row's source path
//! with the same number of trailing components removed. Nothing extra is
//! persisted, and the reconcile pass that completes an interrupted operation
//! finds the same directories.
//!
//! Only `fs::remove_dir` is used: it refuses a directory that is not empty,
//! so this never deletes a file. A folder that still holds something the
//! Move did not take (a symlink, a file written while the move ran) stays,
//! and so do its parents. An empty subfolder is created at the destination
//! before its source is removed, so the moved tree keeps its empty folders.
//! A scan root is never removed, and nothing is removed when the destination
//! lies inside the moved directory. A failure is logged and counted, never
//! returned as a failed operation: the files are already moved.

use crate::db::get_scan_roots;
use crate::file_op::db as fdb;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// What one cleanup pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DirCleanupSummary {
    /// Folders removed at the source.
    pub removed: usize,
    /// Folders left in place (not empty, a scan root, or a failed removal).
    pub kept: usize,
}

/// Remove the source folders a completed Move emptied. Call only once every
/// file row of the operation is done.
pub fn remove_emptied_source_dirs(
    conn: &Connection,
    operation_id: i64,
) -> Result<DirCleanupSummary> {
    let op = fdb::get_operation(conn, operation_id)?;
    let mut summary = DirCleanupSummary::default();
    let Some(dest_dir) = op.dest_dir.as_deref() else {
        return Ok(summary);
    };
    let moved = moved_directories(
        Path::new(dest_dir),
        &fdb::list_operation_files(conn, operation_id)?,
    );
    if moved.is_empty() {
        return Ok(summary);
    }
    let scan_roots: Vec<PathBuf> = get_scan_roots(conn)?
        .into_iter()
        .map(|r| PathBuf::from(r.path))
        .collect();

    for (source_top, dest_top) in &moved {
        if dest_top.starts_with(source_top) {
            tracing::info!(
                operation_id,
                path = %source_top.display(),
                reason = "destination_inside_source",
                "file_op source folder kept"
            );
            summary.kept += 1;
            continue;
        }
        match fs::symlink_metadata(source_top) {
            Ok(md) if md.is_dir() => {}
            // Already gone (a second pass) or replaced by something else:
            // nothing of ours to remove.
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                tracing::warn!(operation_id, path = %source_top.display(), error = %e, "file_op source folder unreadable; kept");
                summary.kept += 1;
                continue;
            }
        }
        clean_tree(
            operation_id,
            source_top,
            dest_top,
            &scan_roots,
            &mut summary,
        );
    }

    tracing::info!(
        operation_id,
        count = summary.removed,
        kept = summary.kept,
        "file_op emptied source folders removed"
    );
    Ok(summary)
}

/// The directories a Move took whole: source directory → its new location.
fn moved_directories(
    dest_dir: &Path,
    files: &[crate::file_op::models::FileOperationFile],
) -> BTreeMap<PathBuf, PathBuf> {
    let mut moved = BTreeMap::new();
    for f in files {
        let Some(dest) = f.dest_path.as_deref() else {
            continue;
        };
        let Ok(rel) = Path::new(dest).strip_prefix(dest_dir) else {
            continue;
        };
        let depth = rel.components().count();
        if depth < 2 {
            // A file selected on its own: its folder was not moved.
            continue;
        }
        let Some(top_name) = rel.components().next().map(|c| c.as_os_str().to_owned()) else {
            continue;
        };
        let Some(source_top) = Path::new(&f.source_path).ancestors().nth(depth - 1) else {
            continue;
        };
        if source_top.file_name() != Some(top_name.as_os_str()) {
            tracing::warn!(
                path = %f.source_path,
                dest = %dest,
                "file_op plan row does not mirror its directory; its folder is not cleaned"
            );
            continue;
        }
        moved.insert(source_top.to_path_buf(), dest_dir.join(&top_name));
    }
    moved
}

/// Remove, deepest first, every empty folder under (and including)
/// `source_top`, recreating each at its place under `dest_top` first.
fn clean_tree(
    operation_id: i64,
    source_top: &Path,
    dest_top: &Path,
    scan_roots: &[PathBuf],
    summary: &mut DirCleanupSummary,
) {
    for entry in WalkDir::new(source_top)
        .follow_links(false)
        .contents_first(true)
    {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(operation_id, path = %source_top.display(), error = %e, "file_op source folder walk failed");
                summary.kept += 1;
                continue;
            }
        };
        if !entry.file_type().is_dir() {
            continue;
        }
        let dir = entry.path();
        if is_scan_root(dir, scan_roots) {
            tracing::info!(operation_id, path = %dir.display(), reason = "scan_root", "file_op source folder kept");
            summary.kept += 1;
            continue;
        }
        match is_empty_dir(dir) {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(operation_id, path = %dir.display(), reason = "not_empty", "file_op source folder kept");
                summary.kept += 1;
                continue;
            }
            Err(e) => {
                tracing::warn!(operation_id, path = %dir.display(), error = %e, "file_op source folder unreadable; kept");
                summary.kept += 1;
                continue;
            }
        }
        let Ok(rel) = dir.strip_prefix(source_top) else {
            continue;
        };
        let target = dest_top.join(rel);
        if let Err(e) = fs::create_dir_all(&target) {
            tracing::warn!(
                operation_id,
                path = %dir.display(),
                dest = %target.display(),
                error = %e,
                "file_op empty folder could not be created at the destination; source kept"
            );
            summary.kept += 1;
            continue;
        }
        match fs::remove_dir(dir) {
            Ok(()) => summary.removed += 1,
            Err(e) => {
                tracing::warn!(operation_id, path = %dir.display(), error = %e, "file_op could not remove an emptied source folder");
                summary.kept += 1;
            }
        }
    }
}

fn is_empty_dir(dir: &Path) -> std::io::Result<bool> {
    Ok(fs::read_dir(dir)?.next().is_none())
}

/// A scan root under either spelling (`/Volumes/x` vs `/private/Volumes/x`).
/// Canonicalizing here is a guard only; no catalog path is matched by it.
fn is_scan_root(dir: &Path, scan_roots: &[PathBuf]) -> bool {
    let dir_canonical = fs::canonicalize(dir).ok();
    scan_roots.iter().any(|root| {
        root.as_path() == dir
            || match (&dir_canonical, fs::canonicalize(root)) {
                (Some(d), Ok(r)) => *d == r,
                _ => false,
            }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use crate::file_op::executor::run_operation;
    use crate::file_op::planner::build_move_plan;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn catalog_with_roots(roots: &[&Path]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        for root in roots {
            conn.execute(
                "INSERT INTO scan_roots (path, enabled, find_duplicates) VALUES (?1, 1, 1)",
                [&root.to_string_lossy().to_string()],
            )
            .unwrap();
        }
        conn
    }

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn run_move(conn: &Connection, sources: Vec<PathBuf>, dest: &Path) {
        let plan = build_move_plan(conn, sources, dest.to_path_buf()).unwrap();
        run_operation(
            conn,
            plan.operation_id,
            &Arc::new(AtomicBool::new(false)),
            &NullEmitter,
        )
        .unwrap();
    }

    #[test]
    fn moving_a_folder_removes_its_emptied_tree_and_keeps_empty_subfolders() {
        let root = TempDir::new().unwrap();
        let conn = catalog_with_roots(&[root.path()]);
        let night = root.path().join("Night");
        write(&night.join("Lights").join("a.fit"), "a");
        write(&night.join("Lights").join("Ha").join("c.fit"), "c");
        write(&night.join("Darks").join("b.fit"), "b");
        fs::create_dir_all(night.join("Empty").join("Deeper")).unwrap();
        let dest = root.path().join("dest");
        fs::create_dir(&dest).unwrap();

        run_move(&conn, vec![night.clone()], &dest);

        assert!(!night.exists(), "the moved folder is gone at the source");
        let moved = dest.join("Night");
        assert_eq!(
            fs::read_to_string(moved.join("Lights").join("a.fit")).unwrap(),
            "a"
        );
        assert_eq!(
            fs::read_to_string(moved.join("Lights").join("Ha").join("c.fit")).unwrap(),
            "c"
        );
        assert_eq!(
            fs::read_to_string(moved.join("Darks").join("b.fit")).unwrap(),
            "b"
        );
        assert!(
            moved.join("Empty").join("Deeper").is_dir(),
            "empty subfolders move too"
        );
    }

    #[test]
    fn a_folder_holding_an_item_the_move_did_not_take_stays_with_its_parents() {
        let root = TempDir::new().unwrap();
        let conn = catalog_with_roots(&[root.path()]);
        let night = root.path().join("Night");
        write(&night.join("Lights").join("a.fit"), "a");
        write(&night.join("Darks").join("b.fit"), "b");
        let dest = root.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let plan = build_move_plan(&conn, vec![night.clone()], dest.clone()).unwrap();
        // Written after the plan froze: the move does not take it.
        write(&night.join("Lights").join("late.txt"), "late");
        run_operation(
            &conn,
            plan.operation_id,
            &Arc::new(AtomicBool::new(false)),
            &NullEmitter,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(night.join("Lights").join("late.txt")).unwrap(),
            "late"
        );
        assert!(
            !night.join("Darks").exists(),
            "an emptied sibling folder is removed"
        );
        assert!(night.is_dir(), "the parent of a kept folder stays");
        assert!(dest.join("Night").join("Lights").join("a.fit").is_file());
        assert!(dest.join("Night").join("Darks").join("b.fit").is_file());
    }

    #[test]
    fn files_selected_on_their_own_leave_their_folder() {
        let root = TempDir::new().unwrap();
        let conn = catalog_with_roots(&[root.path()]);
        let night = root.path().join("Night");
        write(&night.join("a.fit"), "a");
        let dest = root.path().join("dest");
        fs::create_dir(&dest).unwrap();

        run_move(&conn, vec![night.join("a.fit")], &dest);

        assert!(
            night.is_dir(),
            "only a selected folder is removed, not a file's folder"
        );
        assert!(dest.join("a.fit").is_file());
    }

    #[test]
    fn a_scan_root_moved_whole_is_kept() {
        let target_root = TempDir::new().unwrap();
        let source_root = TempDir::new().unwrap();
        let conn = catalog_with_roots(&[target_root.path(), source_root.path()]);
        write(&source_root.path().join("Lights").join("a.fit"), "a");

        run_move(
            &conn,
            vec![source_root.path().to_path_buf()],
            target_root.path(),
        );

        assert!(
            source_root.path().is_dir(),
            "a scan root folder is never removed"
        );
        assert!(
            !source_root.path().join("Lights").exists(),
            "its emptied subfolders are"
        );
        let moved = target_root
            .path()
            .join(source_root.path().file_name().unwrap());
        assert!(moved.join("Lights").join("a.fit").is_file());
    }

    #[test]
    fn a_move_into_its_own_subfolder_removes_nothing() {
        let root = TempDir::new().unwrap();
        let conn = catalog_with_roots(&[root.path()]);
        let folder = root.path().join("A");
        write(&folder.join("x.fit"), "x");
        fs::create_dir_all(folder.join("E")).unwrap();
        let inner = folder.join("B");
        fs::create_dir_all(&inner).unwrap();

        run_move(&conn, vec![folder.clone()], &inner);

        assert!(inner.join("A").join("x.fit").is_file());
        assert!(
            folder.join("E").is_dir(),
            "nothing is removed when the destination is inside the source"
        );
    }
}
