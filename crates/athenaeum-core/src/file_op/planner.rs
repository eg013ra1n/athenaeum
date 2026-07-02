//! Planner for file operations. Takes raw user input (selected paths + dest)
//! and turns it into a fully validated, persisted operation that the executor
//! can run.
//!
//! Move planner responsibilities:
//! - Resolve every source path; reject anything outside all scan roots.
//! - Reject destinations outside all scan roots.
//! - Recursively expand directory selections into individual file rows,
//!   preserving relative paths under the directory's basename.
//! - Detect per-file move strategy (atomic rename vs copy-verify-delete) by
//!   comparing source and destination filesystem device IDs.
//! - Map source path → `files.id` from the catalog (None for non-FITS sidecars).
//! - For cross-volume moves: pre-compute the source xxHash (sampled) so the
//!   executor can verify the destination matches.
//! - Persist `file_operations` + one `file_operation_files` row per file,
//!   then update aggregate totals.

use crate::db::get_scan_roots;
use crate::duplicates::compute_xxhash;
use crate::file_op::db as fdb;
use crate::file_op::models::{FileOpKind, FileOpPlan, MoveStrategy};
use anyhow::{anyhow, Context, Result};
use rusqlite::Connection;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Build and persist a Move plan.
///
/// `sources`: absolute paths the user selected (mix of files and directories OK).
/// `dest_dir`: absolute directory path to move into. Must be inside a scan root.
///
/// On success, returns the operation_id and the plan summary. The operation
/// is persisted with status = `pending` and ready for the executor.
pub fn build_move_plan(
    conn: &Connection,
    sources: Vec<PathBuf>,
    dest_dir: PathBuf,
) -> Result<FileOpPlan> {
    // 1. Validate scan-root containment.
    let scan_roots = get_scan_roots(conn).context("listing scan roots")?;
    let scan_root_paths: Vec<PathBuf> = scan_roots
        .iter()
        .filter(|r| r.enabled)
        .map(|r| PathBuf::from(&r.path))
        .collect();
    if scan_root_paths.is_empty() {
        return Err(anyhow!("no enabled scan roots configured"));
    }

    if !is_inside_any(&dest_dir, &scan_root_paths) {
        return Err(anyhow!(
            "destination '{}' is not inside any scan root",
            dest_dir.display()
        ));
    }
    if !dest_dir.is_dir() {
        return Err(anyhow!(
            "destination '{}' does not exist or is not a directory",
            dest_dir.display()
        ));
    }

    for s in &sources {
        if !is_inside_any(s, &scan_root_paths) {
            return Err(anyhow!(
                "source '{}' is not inside any scan root",
                s.display()
            ));
        }
        if !s.exists() {
            return Err(anyhow!("source '{}' does not exist", s.display()));
        }
    }

    // 2. Expand each top-level source into per-file rows.
    //    For a file: dest = dest_dir / filename.
    //    For a dir:  dest = dest_dir / dir_basename / <relative>.
    #[derive(Debug)]
    struct PlannedFile {
        source: PathBuf,
        dest: PathBuf,
        size: u64,
    }
    let mut planned: Vec<PlannedFile> = Vec::new();
    for src in &sources {
        if src.is_file() {
            let filename = src
                .file_name()
                .ok_or_else(|| anyhow!("source has no filename: {}", src.display()))?;
            let dest = dest_dir.join(filename);
            if dest == *src {
                return Err(anyhow!(
                    "source and destination are the same file: {}",
                    src.display()
                ));
            }
            let size = fs::metadata(src)?.len();
            planned.push(PlannedFile { source: src.clone(), dest, size });
        } else if src.is_dir() {
            let dir_basename = src
                .file_name()
                .ok_or_else(|| anyhow!("directory has no name: {}", src.display()))?;
            let new_dir_root = dest_dir.join(dir_basename);
            if new_dir_root == *src {
                return Err(anyhow!(
                    "source and destination directory are the same: {}",
                    src.display()
                ));
            }
            for entry in WalkDir::new(src).follow_links(false) {
                let entry = entry?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let abs = entry.path();
                let rel = abs.strip_prefix(src)?;
                let dest = new_dir_root.join(rel);
                let size = entry.metadata()?.len();
                planned.push(PlannedFile { source: abs.to_path_buf(), dest, size });
            }
        } else {
            return Err(anyhow!(
                "source is neither a file nor a directory: {}",
                src.display()
            ));
        }
    }

    if planned.is_empty() {
        return Err(anyhow!("plan is empty (no files to move)"));
    }

    // Refuse to move when ANY per-file destination already exists. This is
    // the only safe default: silently overwriting unrelated files at the
    // target — even after a hash-mismatch — would lose data. The user must
    // resolve each conflict manually (delete or rename in the destination)
    // and retry.
    let mut conflicts: Vec<String> = Vec::new();
    for pf in &planned {
        if pf.dest.exists() {
            conflicts.push(pf.dest.to_string_lossy().to_string());
            if conflicts.len() >= 8 {
                break;
            }
        }
    }
    if !conflicts.is_empty() {
        let total_conflicts = planned.iter().filter(|p| p.dest.exists()).count();
        let suffix = if total_conflicts > conflicts.len() {
            format!(" (and {} more)", total_conflicts - conflicts.len())
        } else {
            String::new()
        };
        return Err(anyhow!(
            "destination collision: {} file(s) already exist at the target. Resolve manually then retry.\n  - {}{}",
            total_conflicts,
            conflicts.join("\n  - "),
            suffix
        ));
    }

    // 3. Build catalog file_id lookup for every source path in one shot.
    //    files.path is UNIQUE indexed, so this is fast.
    let catalog_lookup = lookup_catalog_file_ids(
        conn,
        planned.iter().map(|p| p.source.to_string_lossy().to_string()).collect(),
    )?;

    // 4. Detect device IDs per source + dest_dir to choose strategy.
    let dest_dev = device_id_for(&dest_dir)
        .with_context(|| format!("stat destination {}", dest_dir.display()))?;

    // 5. Compute total bytes and the common source root for display.
    let total_bytes: i64 = planned.iter().map(|p| p.size as i64).sum();
    let total_files = planned.len() as i64;
    let source_root = common_root(&sources)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // 6. Persist the operation row.
    let operation_id = fdb::insert_operation(
        conn,
        FileOpKind::Move,
        if source_root.is_empty() {
            None
        } else {
            Some(&source_root)
        },
        Some(&dest_dir.to_string_lossy()),
    )?;

    // 7. Persist one file_operation_files row per planned file.
    for pf in &planned {
        let src_dev = device_id_for(&pf.source)
            .with_context(|| format!("stat source {}", pf.source.display()))?;
        let strategy = if src_dev == dest_dev {
            MoveStrategy::AtomicRename
        } else {
            MoveStrategy::CopyVerifyDelete
        };

        // For cross-volume, pre-compute hash up front so the executor can
        // verify after copy without re-reading the (now possibly-deleted)
        // source. For atomic rename, we don't hash — atomic rename is
        // bit-for-bit by definition.
        let expected_hash = match strategy {
            MoveStrategy::CopyVerifyDelete => Some(
                compute_xxhash(&pf.source)
                    .with_context(|| format!("hashing {}", pf.source.display()))?,
            ),
            _ => None,
        };

        let catalog_file_id = catalog_lookup
            .get(&pf.source.to_string_lossy().to_string())
            .copied();

        fdb::insert_operation_file(
            conn,
            operation_id,
            &pf.source.to_string_lossy(),
            Some(&pf.dest.to_string_lossy()),
            strategy,
            catalog_file_id,
            expected_hash.as_deref(),
            pf.size as i64,
        )?;
    }

    // 8. Update totals on the operation row.
    fdb::update_operation_totals(conn, operation_id, total_files, total_bytes)?;

    // 9. Return the plan summary.
    let files = fdb::list_operation_files(conn, operation_id)?;
    Ok(FileOpPlan {
        operation_id,
        kind: FileOpKind::Move,
        source_root: if source_root.is_empty() {
            None
        } else {
            Some(source_root)
        },
        dest_dir: Some(dest_dir.to_string_lossy().to_string()),
        files,
        total_files,
        total_bytes,
    })
}

fn is_inside_any(p: &Path, roots: &[PathBuf]) -> bool {
    let canonical = match fs::canonicalize(p) {
        Ok(c) => c,
        Err(_) => p.to_path_buf(),
    };
    roots.iter().any(|r| {
        let rc = fs::canonicalize(r).unwrap_or_else(|_| r.clone());
        canonical.starts_with(&rc)
    })
}

#[cfg(unix)]
fn device_id_for(p: &Path) -> Result<u64> {
    use std::os::unix::fs::MetadataExt;
    // Walk up to the nearest existing ancestor (planner may be asked about a
    // dest that doesn't yet exist as a sub-tree). For our planner we already
    // require dest_dir to exist; this is defensive.
    let mut cur: &Path = p;
    loop {
        match fs::metadata(cur) {
            Ok(md) => return Ok(md.dev()),
            Err(_) => match cur.parent() {
                Some(parent) => cur = parent,
                None => return Err(anyhow!("could not stat {} or any ancestor", p.display())),
            },
        }
    }
}

#[cfg(windows)]
fn device_id_for(p: &Path) -> Result<u64> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    // Windows doesn't expose st_dev on stable. Use the volume root (drive
    // letter or UNC \\server\share prefix) of the canonical path as a stable
    // "device key" — same drive → same u64, different drive → different u64,
    // which is all the planner needs to pick AtomicRename vs CopyVerifyDelete.
    let mut cur: &Path = p;
    let canonical = loop {
        if let Ok(c) = fs::canonicalize(cur) {
            break c;
        }
        match cur.parent() {
            Some(parent) => cur = parent,
            None => {
                return Err(anyhow!(
                    "could not canonicalize {} or any ancestor",
                    p.display()
                ))
            }
        }
    };
    let root = canonical
        .components()
        .next()
        .ok_or_else(|| anyhow!("no root component for {}", p.display()))?;
    let mut hasher = DefaultHasher::new();
    root.as_os_str().hash(&mut hasher);
    Ok(hasher.finish())
}

fn common_root(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut iter = paths.iter();
    let first = iter.next()?.clone();
    let mut common = if first.is_dir() {
        first
    } else {
        first.parent()?.to_path_buf()
    };
    for p in iter {
        let candidate = if p.is_dir() {
            p.clone()
        } else {
            match p.parent() {
                Some(parent) => parent.to_path_buf(),
                None => return None,
            }
        };
        while !candidate.starts_with(&common) {
            common = match common.parent() {
                Some(parent) => parent.to_path_buf(),
                None => return None,
            };
        }
    }
    Some(common)
}

fn lookup_catalog_file_ids(
    conn: &Connection,
    paths: Vec<String>,
) -> Result<HashMap<String, i64>> {
    let mut out = HashMap::new();
    if paths.is_empty() {
        return Ok(out);
    }
    // Chunk the IN clause to avoid SQLite's ~999 parameter limit.
    for chunk in paths.chunks(500) {
        let placeholders = std::iter::repeat("?")
            .take(chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, path FROM files WHERE path IN ({})",
            placeholders
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(chunk.iter().map(|s| s.as_str())),
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )?;
        for r in rows {
            let (id, path) = r?;
            out.insert(path, id);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use rusqlite::Connection;
    use std::fs::{self, File};
    use std::io::Write;
    use tempfile::TempDir;

    fn setup_with_scan_root(scan_root: &Path) -> Connection {
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
    fn rejects_source_outside_scan_root() {
        let scan_root = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let conn = setup_with_scan_root(scan_root.path());
        let outside = other.path().join("foo.fit");
        File::create(&outside).unwrap();
        let dest = scan_root.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let err = build_move_plan(&conn, vec![outside], dest).unwrap_err();
        assert!(format!("{}", err).contains("not inside any scan root"));
    }

    #[test]
    fn rejects_destination_outside_scan_root() {
        let scan_root = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let conn = setup_with_scan_root(scan_root.path());
        let src = scan_root.path().join("foo.fit");
        File::create(&src).unwrap();
        let err = build_move_plan(&conn, vec![src], other.path().to_path_buf()).unwrap_err();
        assert!(format!("{}", err).contains("not inside any scan root"));
    }

    #[test]
    fn plans_atomic_rename_for_same_volume_move() {
        let scan_root = TempDir::new().unwrap();
        let conn = setup_with_scan_root(scan_root.path());
        let src = scan_root.path().join("foo.fit");
        let mut f = File::create(&src).unwrap();
        writeln!(f, "test").unwrap();
        let dest = scan_root.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let plan = build_move_plan(&conn, vec![src.clone()], dest).unwrap();
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].strategy, "atomic_rename");
        assert!(plan.files[0].expected_hash.is_none());
    }

    #[test]
    fn rejects_move_when_destination_collides() {
        let scan_root = TempDir::new().unwrap();
        let conn = setup_with_scan_root(scan_root.path());

        // Source: /root/source/foo.fit
        let source_dir = scan_root.path().join("source");
        fs::create_dir(&source_dir).unwrap();
        let src_file = source_dir.join("foo.fit");
        File::create(&src_file).unwrap();

        // Destination already has a foo.fit — collision.
        let dest_dir = scan_root.path().join("dest");
        fs::create_dir(&dest_dir).unwrap();
        File::create(dest_dir.join("foo.fit")).unwrap();

        let err = build_move_plan(&conn, vec![src_file], dest_dir).unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("destination collision"),
            "expected collision error, got: {}",
            msg
        );
        assert!(msg.contains("foo.fit"), "error should name the conflict: {}", msg);
    }

    #[test]
    fn rejects_directory_move_when_any_descendant_collides() {
        let scan_root = TempDir::new().unwrap();
        let conn = setup_with_scan_root(scan_root.path());

        // Source: /root/group/{a.fit, b.fit}
        let group = scan_root.path().join("group");
        fs::create_dir(&group).unwrap();
        File::create(group.join("a.fit")).unwrap();
        File::create(group.join("b.fit")).unwrap();

        // Destination has the directory + one collision (b.fit).
        let dest = scan_root.path().join("organized");
        fs::create_dir(&dest).unwrap();
        let landing = dest.join("group");
        fs::create_dir(&landing).unwrap();
        File::create(landing.join("b.fit")).unwrap();

        let err = build_move_plan(&conn, vec![group], dest).unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("destination collision"),
            "expected collision error, got: {}",
            msg
        );
    }

    #[test]
    fn expands_directory_recursively() {
        let scan_root = TempDir::new().unwrap();
        let conn = setup_with_scan_root(scan_root.path());
        let dir = scan_root.path().join("group");
        fs::create_dir(&dir).unwrap();
        File::create(dir.join("a.fit")).unwrap();
        let nested = dir.join("nested");
        fs::create_dir(&nested).unwrap();
        File::create(nested.join("b.fit")).unwrap();

        let dest = scan_root.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let plan = build_move_plan(&conn, vec![dir.clone()], dest.clone()).unwrap();
        assert_eq!(plan.files.len(), 2);

        // Both should preserve dir_basename + relative path.
        let dest_paths: Vec<String> = plan
            .files
            .iter()
            .map(|f| f.dest_path.clone().unwrap())
            .collect();
        assert!(dest_paths
            .iter()
            .any(|p| p == &dest.join("group").join("a.fit").to_string_lossy()));
        assert!(dest_paths.iter().any(|p| p
            == &dest
                .join("group")
                .join("nested")
                .join("b.fit")
                .to_string_lossy()));
    }
}
