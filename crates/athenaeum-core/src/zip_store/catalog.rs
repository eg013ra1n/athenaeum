//! The zip catalog seen as whole files: what the browser lists, and the path bookkeeping when a zip is
//! renamed, moved or relinked.
//!
//! A zip is one item in the file browser. Nothing here writes into a zip or touches a single entry.
//!
//! Path rules. A foreign row's `files.path` is the virtual path `<zip> + SEP + <entry with SEP>`; one of our
//! rows keeps its original `files.path`. Both carry `archive_zip_path`. Renaming a zip therefore moves
//! `zip_archives.path`, `files.archive_zip_path` and, for the rows whose `files.path` is still the virtual
//! path of the old zip, that path. The last step is idempotent: a folder rename has already rewritten the
//! virtual paths through `db::rename_files_path_prefix` (they start with the folder prefix), and a row in
//! that state no longer matches the old zip's spelling, so it is not touched a second time.
//! `black_hole_zips` and `zip_released_entries` reference `zip_id`, which does not change.
//! `archive_operation_files.target_zip_path` names the zip an operation wrote (unarchive extracts from it,
//! the scanner recognises the operation's entries through it) and moves with the zip.

use std::path::{Path, MAIN_SEPARATOR};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::models::{File, Frame};

/// One zip in a folder listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ZipListItem {
    pub path: String,
    pub size: i64,
    /// Frame entries the catalog holds for this zip.
    pub frame_entries: i64,
    /// The Move and ZIP operation that wrote this zip, when it is one of ours (the browser warns that
    /// removing it removes the only copy of an archived frame set).
    pub created_by_operation: Option<i64>,
}

/// Runs `f` inside a savepoint: nested inside the caller's transaction when there is one, a transaction of
/// its own otherwise. Rolled back whole on an error.
fn in_savepoint<T>(conn: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    conn.execute_batch("SAVEPOINT zip_catalog_rename")?;
    match f() {
        Ok(v) => {
            conn.execute_batch("RELEASE zip_catalog_rename")?;
            Ok(v)
        }
        Err(e) => {
            if let Err(rollback) = conn.execute_batch("ROLLBACK TO zip_catalog_rename; RELEASE zip_catalog_rename") {
                tracing::error!(error = %rollback, "zip rename rollback failed");
            }
            Err(e)
        }
    }
}

/// Whether the catalog knows a zip at exactly this path: a `zip_archives` row, or frames marked with it
/// (`files.archive_zip_path`, which a zip can carry without a row of its own).
pub fn is_known_zip(conn: &Connection, path: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM zip_archives WHERE path = ?1)
             OR EXISTS(SELECT 1 FROM files WHERE archive_zip_path = ?1)",
        [path],
        |r| r.get(0),
    )?)
}

/// Moves one zip's bookkeeping from `old_zip` to `new_zip` (full paths). The caller has already moved the
/// file. Returns the number of `zip_archives` rows renamed (0 or 1). All or nothing.
pub fn rename_zip(conn: &Connection, old_zip: &str, new_zip: &str) -> Result<usize> {
    if old_zip == new_zip {
        return Ok(0);
    }
    in_savepoint(conn, || {
        let moved = move_one_zip(conn, old_zip, new_zip)?;
        conn.execute(
            "UPDATE archive_operation_files SET target_zip_path = ?1 WHERE target_zip_path = ?2",
            params![new_zip, old_zip],
        )
        .with_context(|| format!("repoint the archive operations that wrote {old_zip}"))?;
        Ok(moved)
    })
}

fn move_one_zip(conn: &Connection, old_zip: &str, new_zip: &str) -> Result<usize> {
    let sep = MAIN_SEPARATOR.to_string();
    // Virtual paths still spelled by the old zip. A row a folder rename already moved no longer equals
    // `<old zip> + SEP + <entry>`, so it is skipped: its path is correct exactly once.
    let virtual_moved = conn
        .execute(
            "UPDATE files SET path = ?1 || SUBSTR(path, LENGTH(?2) + 1)
             WHERE archive_zip_path = ?2 AND archive_path_in_zip IS NOT NULL
               AND path = ?2 || ?3 || REPLACE(archive_path_in_zip, '/', ?3)",
            params![new_zip, old_zip, sep],
        )
        .with_context(|| format!("rename the frames of {old_zip}"))?;
    let rows_moved = conn
        .execute("UPDATE files SET archive_zip_path = ?1 WHERE archive_zip_path = ?2", params![new_zip, old_zip])
        .with_context(|| format!("repoint the frames of {old_zip}"))?;
    let zips = conn
        .execute("UPDATE zip_archives SET path = ?1 WHERE path = ?2", params![new_zip, old_zip])
        .with_context(|| format!("rename the zip row of {old_zip}"))?;
    tracing::info!(src = %old_zip, dest = %new_zip, rows = rows_moved, virtual_paths = virtual_moved, "zip renamed in the catalog");
    Ok(zips)
}

/// Moves the bookkeeping of every zip whose path starts with `old_prefix` (a folder path ending in a
/// separator) to the same place under `new_prefix`, like `db::rename_files_path_prefix` does for files: the
/// prefix range is separator-strict and literal. A folder rename calls both; the order does not matter,
/// because the virtual paths are only rewritten where they still carry the old zip's spelling. Returns the
/// number of zips renamed. All or nothing.
pub fn rename_zip_prefix(conn: &Connection, old_prefix: &str, new_prefix: &str) -> Result<usize> {
    if old_prefix == new_prefix {
        return Ok(0);
    }
    in_savepoint(conn, || {
        let zips = zips_under_prefix(conn, old_prefix)?;
        let mut renamed = 0;
        for old in zips {
            let Some(rest) = old.strip_prefix(old_prefix) else {
                tracing::error!(zip_path = %old, prefix = %old_prefix, "zip outside the renamed prefix skipped");
                continue;
            };
            let new = format!("{new_prefix}{rest}");
            renamed += move_one_zip(conn, &old, &new)?;
        }
        // One range update for every operation file whose zip sat under the folder (the column has no index).
        let hi = crate::db::path_prefix_upper(old_prefix);
        conn.execute(
            "UPDATE archive_operation_files SET target_zip_path = ?1 || SUBSTR(target_zip_path, LENGTH(?2) + 1)
             WHERE target_zip_path >= ?2 AND (?3 IS NULL OR target_zip_path < ?3)",
            params![new_prefix, old_prefix, hi],
        )
        .with_context(|| format!("repoint the archive operations that wrote zips under {old_prefix}"))?;
        Ok(renamed)
    })
}

/// Before a folder is renamed on disk: the first catalog path that the rename would collide with, if any. A
/// zip under `old_prefix` whose new path already is a known zip, or a file row (a zip's frame or a plain
/// file) whose renamed path already exists (stale rows under a folder name that is free on disk), would fail the catalog update after the
/// disk rename.
pub fn rename_prefix_collision(conn: &Connection, old_prefix: &str, new_prefix: &str) -> Result<Option<String>> {
    if old_prefix == new_prefix {
        return Ok(None);
    }
    for old in zips_under_prefix(conn, old_prefix)? {
        if let Some(rest) = old.strip_prefix(old_prefix) {
            let new = format!("{new_prefix}{rest}");
            if is_known_zip(conn, &new)? {
                return Ok(Some(new));
            }
        }
    }
    let hi = crate::db::path_prefix_upper(old_prefix);
    let clash = conn
        .query_row(
            "SELECT t.path FROM files f JOIN files t ON t.path = ?1 || SUBSTR(f.path, LENGTH(?2) + 1)
             WHERE f.path >= ?2 AND (?3 IS NULL OR f.path < ?3) LIMIT 1",
            params![new_prefix, old_prefix, hi],
            |r| r.get::<_, String>(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    Ok(clash)
}

/// Every zip path (from `zip_archives`, and from `files.archive_zip_path` for a zip with no row) in the
/// separator-strict range of `prefix`.
fn zips_under_prefix(conn: &Connection, prefix: &str) -> Result<Vec<String>> {
    let hi = crate::db::path_prefix_upper(prefix);
    let mut stmt = conn.prepare(
        "SELECT path FROM zip_archives WHERE path >= ?1 AND (?2 IS NULL OR path < ?2)
         UNION
         SELECT archive_zip_path FROM files
          WHERE archive_zip_path >= ?1 AND (?2 IS NULL OR archive_zip_path < ?2)
         ORDER BY 1",
    )?;
    let rows = stmt.query_map(params![prefix, hi], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// What `relink_zips` found, for the wrong-folder check of a relink.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ZipRelink {
    /// Zips of the old root that exist at the new location and now carry its path (or already did).
    pub present: usize,
    /// Of those, the zips whose catalog rows were moved.
    pub moved: usize,
    /// Zips of the old root that are not at the new location; they stay where they were.
    pub missing: usize,
}

/// After a scan root was relinked from `old_root` to `new_root`: every zip of the old root that exists at
/// the same relative place under the new root follows it. A zip that is not there stays where it was (its
/// frames show as missing) and is counted as missing; one whose new path the catalog already holds is left
/// alone, logged, and counted as present. With the same root twice nothing moves; the zips are only counted.
/// All or nothing: the caller wraps this in its own savepoint with the file relink.
pub fn relink_zips(conn: &Connection, old_root: &str, new_root: &str) -> Result<ZipRelink> {
    let sep = if old_root.starts_with('/') { '/' } else { '\\' };
    let old_prefix = format!("{}{sep}", old_root.trim_end_matches(sep));
    let new_prefix = format!("{}{sep}", new_root.trim_end_matches(sep));
    let same = old_prefix == new_prefix;
    let mut out = ZipRelink::default();
    for old in zips_under_prefix(conn, &old_prefix)? {
        let Some(rest) = old.strip_prefix(&old_prefix) else { continue };
        let new = format!("{new_prefix}{rest}");
        if !Path::new(&new).is_file() {
            out.missing += 1;
            continue;
        }
        out.present += 1;
        if same {
            continue;
        }
        if is_known_zip(conn, &new)? {
            tracing::warn!(src = %old, dest = %new, "relink: the catalog already holds a zip at the new location; left as is");
            continue;
        }
        rename_zip(conn, &old, &new)?;
        out.moved += 1;
    }
    Ok(out)
}

/// The zips directly in `directory` that hold at least one cataloged frame, ordered by path. With
/// `instrume`, only zips holding a frame of that camera. A zip stored with the retry sentinel (size 0 and
/// an empty `modified_at`, left by a failed entry) shows its live size instead; when the stat fails it is
/// still listed, with size 0.
pub fn zips_in_directory(conn: &Connection, directory: &str, instrume: Option<&str>) -> Result<Vec<ZipListItem>> {
    let sep = crate::db::native_separator_of(directory).to_string();
    let dir = directory.trim_end_matches(['/', '\\']);
    let prefix = format!("{dir}{sep}");
    let hi = crate::db::path_prefix_upper(&prefix);
    let depth = dir.matches(sep.as_str()).count() as i64 + 1;
    let mut stmt = conn.prepare(
        "SELECT z.path, z.size, z.modified_at, z.frame_entries, z.created_by_operation FROM zip_archives z
         WHERE z.path >= ?1 AND (?2 IS NULL OR z.path < ?2)
           AND (LENGTH(z.path) - LENGTH(REPLACE(z.path, ?3, ''))) = ?4
           AND z.frame_entries > 0
           AND (?5 IS NULL OR EXISTS (
                 SELECT 1 FROM files f JOIN frames fr ON fr.file_id = f.id
                 WHERE f.archive_zip_path = z.path AND fr.instrume = ?5))
         ORDER BY z.path",
    )?;
    let rows = stmt
        .query_map(params![prefix, hi, sep, depth, instrume], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (path, mut size, modified_at, frame_entries, created_by_operation) in rows {
        if size == 0 && modified_at.is_empty() {
            size = match std::fs::metadata(&path) {
                Ok(m) => i64::try_from(m.len()).unwrap_or(i64::MAX),
                Err(e) => {
                    tracing::warn!(zip_path = %path, error = %e, "zip size unavailable for the listing");
                    0
                }
            };
        }
        out.push(ZipListItem { path, size, frame_entries, created_by_operation });
    }
    Ok(out)
}

/// The cataloged rows that belong to `zip`: those marked with it (`archive_zip_path`, ours and foreign) and
/// those whose `files.path` is a virtual path under it. Each with its frame, ordered by file name.
pub fn rows_of_zip(conn: &Connection, zip: &str) -> Result<Vec<(File, Option<Frame>)>> {
    let prefix = format!("{zip}{MAIN_SEPARATOR}");
    let hi = crate::db::path_prefix_upper(&prefix);
    let sql = format!(
        "SELECT {select} FROM files f LEFT JOIN frames fr ON f.id = fr.file_id
         WHERE f.archive_zip_path = ?1 OR (f.path >= ?2 AND (?3 IS NULL OR f.path < ?3))
         ORDER BY f.filename",
        select = crate::db::FILE_FRAME_SELECT,
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![zip, prefix, hi], crate::db::map_file_frame_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::models::ArchiveCompression;
    use crate::db::schema::init_db;
    use crate::zip_store::scan::scan_zip;
    use crate::zip_store::scan::test_support::{light, make_zip};
    use tempfile::TempDir;

    fn catalog(root: &Path) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [root.to_str().unwrap()]).unwrap();
        conn
    }

    fn paths(conn: &Connection) -> Vec<(String, Option<String>)> {
        conn.prepare("SELECT path, archive_zip_path FROM files ORDER BY path")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn zip_with_two_lights(conn: &Connection, dir: &Path, name: &str, seed: u32) -> std::path::PathBuf {
        let zip = make_zip(
            dir,
            name,
            &[("Lights/L_001.fits", light(seed)), ("Lights/L_002.fits", light(seed + 1))],
            ArchiveCompression::Store,
        );
        scan_zip(conn, &zip, 1, false).unwrap();
        zip
    }

    #[test]
    fn rename_zip_moves_row_markers_and_virtual_paths_once() {
        let tmp = TempDir::new().unwrap();
        let conn = catalog(tmp.path());
        let old = zip_with_two_lights(&conn, tmp.path(), "night.zip", 1);
        let new = tmp.path().join("renamed.zip");
        let (old_s, new_s) = (old.to_string_lossy().into_owned(), new.to_string_lossy().into_owned());
        let sep = MAIN_SEPARATOR;
        assert_eq!(rename_zip(&conn, &old_s, &new_s).unwrap(), 1);
        let got = paths(&conn);
        assert_eq!(
            got,
            vec![
                (format!("{new_s}{sep}Lights{sep}L_001.fits"), Some(new_s.clone())),
                (format!("{new_s}{sep}Lights{sep}L_002.fits"), Some(new_s.clone())),
            ]
        );
        let zip_paths: Vec<String> = conn
            .prepare("SELECT path FROM zip_archives")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(zip_paths, vec![new_s.clone()]);
        // Renaming a zip the catalog does not know is a no-op, not an error.
        assert_eq!(rename_zip(&conn, &old_s, &new_s).unwrap(), 0);
    }

    #[test]
    fn rename_zip_prefix_after_the_file_prefix_swap_does_not_double_the_prefix() {
        let tmp = TempDir::new().unwrap();
        let conn = catalog(tmp.path());
        let sub = tmp.path().join("Old");
        std::fs::create_dir(&sub).unwrap();
        let zip = zip_with_two_lights(&conn, &sub, "night.zip", 1);
        let sep = MAIN_SEPARATOR;
        let old_prefix = format!("{}{sep}", sub.to_string_lossy());
        let new_prefix = format!("{}{sep}", tmp.path().join("New").to_string_lossy());

        // The folder rename's order: the file prefix swap first (it moves the virtual paths), then the zips.
        crate::db::rename_files_path_prefix(&conn, &old_prefix, &new_prefix).unwrap();
        assert_eq!(rename_zip_prefix(&conn, &old_prefix, &new_prefix).unwrap(), 1);
        let new_zip = format!("{new_prefix}night.zip");
        let want: Vec<(String, Option<String>)> = ["L_001.fits", "L_002.fits"]
            .iter()
            .map(|n| (format!("{new_zip}{sep}Lights{sep}{n}"), Some(new_zip.clone())))
            .collect();
        assert_eq!(paths(&conn), want);
        let _ = zip;

        // The other order gives the same rows (and a second run changes nothing).
        let conn2 = catalog(tmp.path());
        let sub2 = tmp.path().join("Old2");
        std::fs::create_dir(&sub2).unwrap();
        zip_with_two_lights(&conn2, &sub2, "night.zip", 5);
        let op = format!("{}{sep}", sub2.to_string_lossy());
        let np = format!("{}{sep}", tmp.path().join("New2").to_string_lossy());
        assert_eq!(rename_zip_prefix(&conn2, &op, &np).unwrap(), 1);
        crate::db::rename_files_path_prefix(&conn2, &op, &np).unwrap();
        let nz = format!("{np}night.zip");
        let want2: Vec<(String, Option<String>)> = ["L_001.fits", "L_002.fits"]
            .iter()
            .map(|n| (format!("{nz}{sep}Lights{sep}{n}"), Some(nz.clone())))
            .collect();
        assert_eq!(paths(&conn2), want2);
        assert_eq!(rename_zip_prefix(&conn2, &op, &np).unwrap(), 0);
    }

    #[test]
    fn rename_zip_prefix_is_separator_strict() {
        let tmp = TempDir::new().unwrap();
        let conn = catalog(tmp.path());
        for p in ["/data/M31/a.zip", "/data/M31_Ha/b.zip", "/data/m31/c.zip"] {
            conn.execute(
                "INSERT INTO zip_archives (path, size, modified_at, frame_entries, scanned_at) VALUES (?1, 1, 't', 1, 't')",
                [p],
            )
            .unwrap();
        }
        assert_eq!(rename_zip_prefix(&conn, "/data/M31/", "/data/N/").unwrap(), 1);
        let got: Vec<String> = conn
            .prepare("SELECT path FROM zip_archives ORDER BY path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(got, vec!["/data/M31_Ha/b.zip", "/data/N/a.zip", "/data/m31/c.zip"]);
    }

    #[test]
    fn rename_zip_failure_leaves_everything_as_it_was() {
        let tmp = TempDir::new().unwrap();
        let conn = catalog(tmp.path());
        let a = zip_with_two_lights(&conn, tmp.path(), "a.zip", 1);
        // b.zip's entries are named differently, so the first two statements of the rename succeed and only
        // the zip_archives UNIQUE (the third) fails: the savepoint has real work to undo.
        let b = make_zip(
            tmp.path(),
            "b.zip",
            &[("Lights/B_001.fits", light(10)), ("Lights/B_002.fits", light(11))],
            ArchiveCompression::Store,
        );
        scan_zip(&conn, &b, 1, false).unwrap();
        let before = paths(&conn);
        let err = rename_zip(&conn, &a.to_string_lossy(), &b.to_string_lossy());
        assert!(err.is_err());
        assert!(format!("{:#}", err.unwrap_err()).contains("zip row"), "the zip_archives statement is the one that failed");
        assert_eq!(paths(&conn), before, "the failed rename rolled back whole");
    }

    fn insert_operation_file(conn: &Connection, zip: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO archive_operations (id, archive_root_path, compression, status, started_at)
             VALUES (1, '/arch', 'store', 'completed', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO archive_operation_files (operation_id, source_path, target_zip_path, target_path_in_zip, expected_hash, disposition, frame_role, file_size_bytes)
             VALUES (1, 's', ?1, 'e', 'h', 'move', 'light', 1)",
            [zip],
        )
        .unwrap();
    }

    fn operation_zips(conn: &Connection) -> Vec<String> {
        conn.prepare("SELECT target_zip_path FROM archive_operation_files ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn rename_zip_repoints_the_operation_files_that_name_it() {
        let tmp = TempDir::new().unwrap();
        let conn = catalog(tmp.path());
        let old = zip_with_two_lights(&conn, tmp.path(), "night.zip", 1);
        let other = zip_with_two_lights(&conn, tmp.path(), "other.zip", 10);
        let (old_s, other_s) = (old.to_string_lossy().into_owned(), other.to_string_lossy().into_owned());
        insert_operation_file(&conn, &old_s);
        insert_operation_file(&conn, &other_s);
        let new_s = tmp.path().join("renamed.zip").to_string_lossy().into_owned();
        rename_zip(&conn, &old_s, &new_s).unwrap();
        assert_eq!(operation_zips(&conn), vec![new_s, other_s], "only the renamed zip's operation files move");
    }

    #[test]
    fn rename_zip_prefix_repoints_the_operation_files_under_the_folder() {
        let tmp = TempDir::new().unwrap();
        let conn = catalog(tmp.path());
        for p in ["/data/M31/a.zip", "/data/M31/sub/b.zip", "/data/M31_Ha/c.zip", "/data/m31/d.zip"] {
            insert_operation_file(&conn, p);
        }
        rename_zip_prefix(&conn, "/data/M31/", "/data/N/").unwrap();
        assert_eq!(
            operation_zips(&conn),
            vec!["/data/N/a.zip", "/data/N/sub/b.zip", "/data/M31_Ha/c.zip", "/data/m31/d.zip"]
        );
    }

    #[test]
    fn a_zip_known_only_through_its_frames_is_known_and_renamed() {
        let tmp = TempDir::new().unwrap();
        let conn = catalog(tmp.path());
        let zip = zip_with_two_lights(&conn, tmp.path(), "night.zip", 1);
        conn.execute("DELETE FROM zip_archives", []).unwrap();
        let zip_s = zip.to_string_lossy().into_owned();
        assert!(is_known_zip(&conn, &zip_s).unwrap(), "frames marked with the zip make it known");
        let new_s = tmp.path().join("renamed.zip").to_string_lossy().into_owned();
        assert_eq!(rename_zip(&conn, &zip_s, &new_s).unwrap(), 0, "no zip_archives row to rename");
        assert!(paths(&conn).iter().all(|(p, z)| p.starts_with(&new_s) && z.as_deref() == Some(new_s.as_str())));
        assert!(!is_known_zip(&conn, &zip_s).unwrap());
    }

    #[test]
    fn rename_prefix_collision_names_what_the_folder_rename_would_hit() {
        let tmp = TempDir::new().unwrap();
        let conn = catalog(tmp.path());
        let sep = MAIN_SEPARATOR;
        let old_dir = tmp.path().join("Old");
        std::fs::create_dir(&old_dir).unwrap();
        zip_with_two_lights(&conn, &old_dir, "night.zip", 1);
        let (op, np) = (format!("{}{sep}", old_dir.to_string_lossy()), format!("{}{sep}", tmp.path().join("New").to_string_lossy()));
        assert_eq!(rename_prefix_collision(&conn, &op, &np).unwrap(), None);

        // A stale frame row under the new name, at the path a frame of the zip would be renamed to.
        let stale = format!("{np}night.zip{sep}Lights{sep}L_001.fits");
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'L_001.fits', 1, 't', 'FITS')",
            [&stale],
        )
        .unwrap();
        assert_eq!(rename_prefix_collision(&conn, &op, &np).unwrap(), Some(stale));
        conn.execute("DELETE FROM files WHERE archive_zip_path IS NULL", []).unwrap();

        // A stale zip row at the new name.
        let stale_zip = format!("{np}night.zip");
        conn.execute(
            "INSERT INTO zip_archives (path, size, modified_at, frame_entries, scanned_at) VALUES (?1, 1, 't', 1, 't')",
            [&stale_zip],
        )
        .unwrap();
        assert_eq!(rename_prefix_collision(&conn, &op, &np).unwrap(), Some(stale_zip.clone()));
        conn.execute("DELETE FROM zip_archives WHERE path = ?1", [&stale_zip]).unwrap();
        assert_eq!(rename_prefix_collision(&conn, &op, &np).unwrap(), None);

        // A plain file under the old folder (not in a zip) and a stale plain-file row at the path it would
        // be renamed to: the folder rename would fail on files.path after the disk rename.
        let plain_old = format!("{op}L_900.fits");
        let plain_new = format!("{np}L_900.fits");
        for p in [&plain_old, &plain_new] {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'L_900.fits', 1, 't', 'FITS')",
                [p],
            )
            .unwrap();
        }
        assert_eq!(rename_prefix_collision(&conn, &op, &np).unwrap(), Some(plain_new));
    }

    #[test]
    fn zips_in_directory_lists_direct_children_with_frames_only() {
        let tmp = TempDir::new().unwrap();
        let conn = catalog(tmp.path());
        let with_frames = zip_with_two_lights(&conn, tmp.path(), "night.zip", 1);
        let deep = tmp.path().join("deep");
        std::fs::create_dir(&deep).unwrap();
        zip_with_two_lights(&conn, &deep, "inner.zip", 20);
        // A zip with no frame entries (a scanned archive of other files) is not listed.
        conn.execute(
            "INSERT INTO zip_archives (path, size, modified_at, frame_entries, scanned_at) VALUES (?1, 9, 't', 0, 't')",
            [tmp.path().join("docs.zip").to_str().unwrap()],
        )
        .unwrap();
        let listed = zips_in_directory(&conn, tmp.path().to_str().unwrap(), None).unwrap();
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].path, with_frames.to_string_lossy());
        assert_eq!(listed[0].frame_entries, 2);
        assert_eq!(listed[0].size, std::fs::metadata(&with_frames).unwrap().len() as i64);
        let in_deep = zips_in_directory(&conn, deep.to_str().unwrap(), None).unwrap();
        assert_eq!(in_deep.len(), 1);
        // A trailing separator on the directory lists the same zips.
        let slash = format!("{}{MAIN_SEPARATOR}", tmp.path().to_string_lossy());
        assert_eq!(zips_in_directory(&conn, &slash, None).unwrap(), listed);
        // The camera filter: the fixture frames are INSTRUME 'C'.
        assert_eq!(zips_in_directory(&conn, tmp.path().to_str().unwrap(), Some("C")).unwrap().len(), 1);
        assert!(zips_in_directory(&conn, tmp.path().to_str().unwrap(), Some("Other")).unwrap().is_empty());
    }

    #[test]
    fn a_retry_sentinel_zip_shows_its_live_size_and_is_never_hidden() {
        let tmp = TempDir::new().unwrap();
        let conn = catalog(tmp.path());
        let zip = zip_with_two_lights(&conn, tmp.path(), "night.zip", 1);
        conn.execute("UPDATE zip_archives SET size = 0, modified_at = ''", []).unwrap();
        let live = std::fs::metadata(&zip).unwrap().len() as i64;
        let listed = zips_in_directory(&conn, tmp.path().to_str().unwrap(), None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].size, live, "the sentinel is not shown as a real size");

        // The zip cannot be statted: still listed, size 0.
        std::fs::remove_file(&zip).unwrap();
        let listed = zips_in_directory(&conn, tmp.path().to_str().unwrap(), None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].size, 0);
    }

    #[test]
    fn relink_zips_follows_only_zips_that_exist_at_the_new_root() {
        let old_root = TempDir::new().unwrap();
        let new_root = TempDir::new().unwrap();
        let conn = catalog(old_root.path());
        let present = zip_with_two_lights(&conn, old_root.path(), "present.zip", 1);
        zip_with_two_lights(&conn, old_root.path(), "absent.zip", 10);
        std::fs::copy(&present, new_root.path().join("present.zip")).unwrap();
        let (present_s, absent_s) = (
            old_root.path().join("present.zip").to_string_lossy().into_owned(),
            old_root.path().join("absent.zip").to_string_lossy().into_owned(),
        );
        insert_operation_file(&conn, &present_s);
        insert_operation_file(&conn, &absent_s);
        let found = relink_zips(&conn, old_root.path().to_str().unwrap(), new_root.path().to_str().unwrap()).unwrap();
        assert_eq!(found, ZipRelink { present: 1, moved: 1, missing: 1 });
        assert_eq!(
            operation_zips(&conn),
            vec![new_root.path().join("present.zip").to_string_lossy().into_owned(), absent_s],
            "the operation that wrote the relinked zip finds it"
        );
        let new_zip = new_root.path().join("present.zip").to_string_lossy().into_owned();
        let rows = paths(&conn);
        let sep = MAIN_SEPARATOR;
        assert!(rows.contains(&(format!("{new_zip}{sep}Lights{sep}L_001.fits"), Some(new_zip.clone()))), "{rows:?}");
        let absent = old_root.path().join("absent.zip").to_string_lossy().into_owned();
        assert!(rows.iter().any(|(p, z)| p.starts_with(&absent) && z.as_deref() == Some(absent.as_str())), "{rows:?}");
        assert_eq!(rows.len(), 4, "no row was deleted");
    }
}
