/// Module for relinking files when directories move
///
/// This module provides functionality to update file paths in the database when
/// monitored directories change location. Files are matched by their FITS header
/// fingerprints to ensure accurate relinking even when filenames or directory
/// structures change.
use crate::fingerprint::compute_header_fingerprint;
use crate::fits_parser::{extract_fits_header, extract_xisf_header};
use crate::models::{FileFormat, RelinkResult};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use walkdir::WalkDir;

/// Relink files from an old scan root path to a new location
///
/// This function:
/// 1. Builds a map of fingerprints -> (file_id, old_path) for files under old root
/// 2. Scans the new directory to find FITS/XISF files
/// 3. Matches files by their header fingerprints
/// 4. Updates file paths in the database for matched files
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `old_root_path` - The original scan root path (files with this prefix will be relinked)
/// * `new_root_path` - The new location where files have been moved
///
/// # Returns
///
/// `RelinkResult` with statistics about matched, new, and orphaned files
pub fn relink_files(
    conn: &Connection,
    old_root_path: &str,
    new_root_path: &str,
) -> Result<RelinkResult> {
    tracing::info!(src = %old_root_path, dest = %new_root_path, "starting relink");

    // Check if paths are the same - run verification mode instead
    if old_root_path == new_root_path {
        tracing::info!(
            path = %old_root_path,
            "src and dest identical, running verification instead of relink"
        );
        return verify_files_at_location(conn, old_root_path);
    }

    // Step 1: Build fingerprint map for files under old root
    let mut fingerprint_map: HashMap<String, (i64, String)> = HashMap::new();

    // Separator-strict byte-range prefix (same helper as every destructive
    // root-scoped site since 81aedae7): a name-prefix sibling root
    // (/data/M31_Ha), a case-variant root (/data/m31 — LIKE is ASCII
    // case-insensitive), or a `_`/`%` in the root name can no longer pull
    // foreign rows into the fingerprint map and get their paths rewritten.
    let (pred, values) =
        crate::db::scan_root_prefix_predicate("f.path", &[old_root_path.to_string()]);
    let sql = format!(
        "SELECT f.id, f.path, f.filename, fh.header_fingerprint
         FROM files f
         INNER JOIN fits_header fh ON f.id = fh.file_id
         WHERE ({pred}) AND fh.header_fingerprint IS NOT NULL"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values.iter()), |row| {
        Ok((
            row.get::<_, i64>(0)?,    // id
            row.get::<_, String>(1)?, // path
            row.get::<_, String>(2)?, // filename
            row.get::<_, String>(3)?, // header_fingerprint
        ))
    })?;

    for row in rows {
        let (file_id, path, _filename, fingerprint) = row?;
        fingerprint_map.insert(fingerprint, (file_id, path));
    }

    tracing::info!(
        src = %old_root_path,
        count = fingerprint_map.len(),
        "found files with fingerprints under old root"
    );

    // Step 2: Scan new directory for FITS/XISF files
    let new_root = Path::new(new_root_path);
    if !new_root.exists() {
        anyhow::bail!("New root path does not exist: {}", new_root_path);
    }

    let mut files_matched = 0;
    let mut files_new = 0;
    let mut matched_file_ids = std::collections::HashSet::new();

    // max_depth caps recursion in case follow_links hits a pathological
    // symlink loop (walkdir's loop detection isn't bulletproof on every
    // filesystem); 64 is well past any realistic archive.
    for entry in WalkDir::new(new_root)
        .follow_links(true)
        .max_depth(64)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase());

        let format = match extension.as_deref() {
            Some("fits") | Some("fit") | Some("fts") => FileFormat::FITS,
            Some("xisf") => FileFormat::XISF,
            _ => continue,
        };

        // Extract header and compute fingerprint
        let header = match format {
            FileFormat::FITS => match extract_fits_header(path) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to read FITS header, skipping file");
                    continue;
                }
            },
            FileFormat::XISF => match extract_xisf_header(path) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to read XISF header, skipping file");
                    continue;
                }
            },
        };

        let fingerprint = compute_header_fingerprint(&header);

        // Try to match with old files
        if let Some((file_id, old_path)) = fingerprint_map.get(&fingerprint) {
            // Same invariant as the scanner (path_to_utf8): a U+FFFD-mangled
            // path would break every later exact/prefix lookup and std::fs open.
            let new_path_str = match crate::scanner::path_to_utf8(path) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping non-UTF-8 path during relink");
                    continue;
                }
            };
            let new_filename = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| new_path_str.clone());

            // Update the file path in database
            conn.execute(
                "UPDATE files SET path = ?1, filename = ?2 WHERE id = ?3",
                params![new_path_str, new_filename, file_id],
            )
            .context("Failed to update file path")?;

            tracing::debug!(
                file_id = *file_id,
                src = %old_path,
                dest = %new_path_str,
                "matched file to new location"
            );
            files_matched += 1;
            matched_file_ids.insert(*file_id);
        } else {
            // File not in database
            files_new += 1;
        }
    }

    // Step 3: Identify orphaned files (in DB but not found in new location)
    let orphaned_file_ids: Vec<i64> = fingerprint_map
        .values()
        .filter(|(file_id, _)| !matched_file_ids.contains(file_id))
        .map(|(file_id, _)| *file_id)
        .collect();

    let files_orphaned = orphaned_file_ids.len();

    tracing::info!(
        matched = files_matched,
        new_files = files_new,
        orphaned = files_orphaned,
        "relinking complete"
    );

    Ok(RelinkResult {
        files_matched,
        files_new,
        files_orphaned,
        orphaned_file_ids,
    })
}

/// Verify files at a location (used when relinking to same directory)
///
/// This checks which files in the database still exist on disk at the current location.
/// Returns a result showing how many files are still present vs. orphaned.
fn verify_files_at_location(conn: &Connection, root_path: &str) -> Result<RelinkResult> {
    let mut fingerprint_map: HashMap<String, (i64, String)> = HashMap::new();

    // Separator-strict byte-range prefix (same helper as every destructive
    // root-scoped site since 81aedae7): a name-prefix sibling root
    // (/data/M31_Ha), a case-variant root (/data/m31 — LIKE is ASCII
    // case-insensitive), or a `_`/`%` in the root name can no longer pull
    // foreign rows into the fingerprint map and get reported as orphans of
    // this root (which the caller may then delete from the catalog).
    let (pred, values) = crate::db::scan_root_prefix_predicate("f.path", &[root_path.to_string()]);
    let sql = format!(
        "SELECT f.id, f.path, f.filename, fh.header_fingerprint
         FROM files f
         INNER JOIN fits_header fh ON f.id = fh.file_id
         WHERE ({pred}) AND fh.header_fingerprint IS NOT NULL"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values.iter()), |row| {
        Ok((
            row.get::<_, i64>(0)?,    // id
            row.get::<_, String>(1)?, // path
            row.get::<_, String>(2)?, // filename
            row.get::<_, String>(3)?, // header_fingerprint
        ))
    })?;

    for row in rows {
        let (file_id, path, _filename, fingerprint) = row?;
        fingerprint_map.insert(fingerprint, (file_id, path));
    }

    let total_files = fingerprint_map.len();
    tracing::info!(path = %root_path, count = total_files, "found files with fingerprints at location");

    let mut files_found = 0;
    let mut missing_file_ids = Vec::new();

    // Check which files still exist on disk
    for (_fingerprint, (file_id, path)) in fingerprint_map.iter() {
        if std::path::Path::new(path).exists() {
            files_found += 1;
        } else {
            missing_file_ids.push(*file_id);
        }
    }

    let files_missing = missing_file_ids.len();

    tracing::info!(
        found = files_found,
        missing = files_missing,
        "verification complete"
    );

    Ok(RelinkResult {
        files_matched: files_found,
        files_new: 0, // No new files when verifying same location
        files_orphaned: files_missing,
        orphaned_file_ids: missing_file_ids,
    })
}

/// Get details about orphaned files for user review
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `file_ids` - List of orphaned file IDs
///
/// # Returns
///
/// Vector of `OrphanedFile` structs with file details
pub fn get_orphaned_file_details(
    conn: &Connection,
    file_ids: &[i64],
) -> Result<Vec<crate::models::OrphanedFile>> {
    if file_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let query = format!(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at,
                EXISTS(SELECT 1 FROM frames fr WHERE fr.file_id = f.id) as has_frame,
                (SELECT fr.object FROM frames fr WHERE fr.file_id = f.id LIMIT 1) as object,
                (SELECT fr.date_obs FROM frames fr WHERE fr.file_id = f.id LIMIT 1) as date_obs
         FROM files f
         WHERE f.id IN ({})",
        placeholders
    );

    let mut stmt = conn.prepare(&query)?;
    let params: Vec<&dyn rusqlite::ToSql> = file_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok(crate::models::OrphanedFile {
            id: row.get(0)?,
            path: row.get(1)?,
            filename: row.get(2)?,
            size: row.get(3)?,
            modified_at: row.get(4)?,
            has_frame: row.get::<_, i64>(5)? != 0,
            object: row.get(6).ok(),
            date_obs: row.get(7).ok(),
        })
    })?;

    let mut orphaned_files = Vec::new();
    for row in rows {
        orphaned_files.push(row?);
    }

    Ok(orphaned_files)
}

/// Delete orphaned files from the database
///
/// A purged file is one the user has confirmed is gone from disk, so a master
/// library file arriving here gets full `delete_master` semantics: its raw
/// source set is un-superseded and its consumers repointed before the row goes
/// (2026-08-02 audit C3). Without that, the raw frames stay invisible to the
/// matcher forever — superseded by a master that exists neither on disk nor in
/// the catalog, with nothing left in the UI to undo it.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `file_ids` - List of file IDs to delete
///
/// # Returns
///
/// Number of files deleted
pub fn delete_orphaned_files(conn: &Connection, file_ids: &[i64]) -> Result<usize> {
    if file_ids.is_empty() {
        return Ok(0);
    }

    // Per-file, and per-file-fallible: one file whose unregister fails is
    // dropped from the purge (its row survives, so the lineage is still
    // undoable) rather than aborting the whole batch or being stranded. A failed
    // LOOKUP is logged and the file treated as ordinary — same stance as the
    // Black Hole paths: refusing a purge because one SELECT failed is worse than
    // the stranding this guards against.
    let mut deletable: Vec<i64> = Vec::with_capacity(file_ids.len());
    for &file_id in file_ids {
        match crate::db::master_unregister::master_set_id_for_file(conn, file_id) {
            Ok(Some(master_set_id)) => {
                match crate::db::master_unregister::unregister_master_set(conn, master_set_id) {
                    Ok(_) => tracing::info!(
                        file_id,
                        master_set_id,
                        "master unregistered before orphan purge"
                    ),
                    Err(e) => {
                        tracing::error!(file_id, master_set_id, error = %e,
                            "failed to unregister master before orphan purge — file kept");
                        continue;
                    }
                }
            }
            Ok(None) => {}
            Err(e) => tracing::error!(file_id, error = %e, "master lookup before orphan purge failed"),
        }
        deletable.push(file_id);
    }
    if deletable.is_empty() {
        return Ok(0);
    }

    let placeholders = deletable.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let query = format!("DELETE FROM files WHERE id IN ({})", placeholders);

    let params: Vec<&dyn rusqlite::ToSql> = deletable
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let deleted = conn.execute(&query, params.as_slice())?;

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orphaned_file_details_empty() {
        let conn = Connection::open_in_memory().unwrap();
        let result = get_orphaned_file_details(&conn, &[]).unwrap();
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_delete_orphaned_files_empty() {
        let conn = Connection::open_in_memory().unwrap();
        let result = delete_orphaned_files(&conn, &[]).unwrap();
        assert_eq!(result, 0);
    }

    /// Purging a missing master's row must un-supersede the raw set it replaced
    /// (2026-08-02 audit C3) — otherwise those raw frames stay invisible to the
    /// matcher forever, superseded by a master that is gone from disk AND from
    /// the catalog.
    #[test]
    fn orphan_purge_unregisters_a_purged_master() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();

        let seed_file = |path: &str, imagetyp: &str| -> i64 {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, ?1, 100, '2026-08-01T00:00:00Z', 'FITS')",
                params![path],
            )
            .unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp) VALUES (?1, ?2)",
                params![file_id, imagetyp],
            )
            .unwrap();
            file_id
        };

        // The state register_master leaves behind: raw set superseded by a
        // master that owns one (now missing) file and inherited the consumer link.
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-08-01')",
            [],
        )
        .unwrap();
        let raw_set_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, is_master_library)
             VALUES ('MasterDark', '2026-08-01', 1)",
            [],
        )
        .unwrap();
        let master_set_id = conn.last_insert_rowid();
        conn.execute(
            "UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2",
            params![master_set_id, raw_set_id],
        )
        .unwrap();

        let master_file_id = seed_file("/lib/master_dark.fits", "MASTERDARK");
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id)
             VALUES (?1, (SELECT id FROM frames WHERE file_id = ?2))",
            params![master_set_id, master_file_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO master_provenance
             (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
             VALUES (?1, ?2, '{}', '[]', 'h', '2026-08-02T00:00:00Z')",
            params![master_set_id, raw_set_id],
        )
        .unwrap();

        let consumer_file_id = seed_file("/lib/light.fits", "LIGHT");
        let consumer_frame_id: i64 = conn
            .query_row(
                "SELECT id FROM frames WHERE file_id = ?1",
                params![consumer_file_id],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type)
             VALUES (?1, 'frame', ?2, 'Dark')",
            params![consumer_frame_id, master_set_id],
        )
        .unwrap();

        let deleted = delete_orphaned_files(&conn, &[master_file_id]).unwrap();
        assert_eq!(deleted, 1, "the purged file row is gone");

        let sup: Option<i64> = conn
            .query_row(
                "SELECT superseded_by_set_id FROM calibration_set WHERE id = ?1",
                [raw_set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sup, None, "raw set is matchable again");

        let target: i64 = conn
            .query_row(
                "SELECT calibration_set_id FROM calibration_set_to_frames
                  WHERE source_id = ?1 AND source_type = 'frame'",
                [consumer_frame_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, raw_set_id, "consumer link repointed to the raw set");

        let masters: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set WHERE id = ?1",
                [master_set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(masters, 0, "master shell row is gone");
    }

    /// The guard is inert for ordinary files: purging a plain orphan neither
    /// touches its calibration set nor stops at the master lookup.
    #[test]
    fn orphan_purge_leaves_ordinary_files_alone() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-08-01')",
            [],
        )
        .unwrap();
        let set_id = conn.last_insert_rowid();

        let mut ids = Vec::new();
        for name in ["/lib/d1.fits", "/lib/d2.fits"] {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, ?1, 100, '2026-08-01T00:00:00Z', 'FITS')",
                params![name],
            )
            .unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp) VALUES (?1, 'DARK')",
                params![file_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                params![set_id, conn.last_insert_rowid()],
            )
            .unwrap();
            ids.push(file_id);
        }

        assert_eq!(delete_orphaned_files(&conn, &ids[..1]).unwrap(), 1);

        let alive: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set WHERE id = ?1",
                [set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(alive, 1, "an ordinary calibration set is never unregistered");
    }

    #[test]
    fn relink_does_not_sweep_sibling_or_case_variant_roots() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();

        // Real tiny FITS in the NEW location so the walk yields a fingerprint.
        let dir = tempfile::tempdir().unwrap();
        let new_root = dir.path().join("relocated");
        std::fs::create_dir_all(&new_root).unwrap();
        let walked = new_root.join("x.fits");
        crate::fits_writer::write_fits_f32(&walked, 4, 4, 1, &vec![0.0f32; 16], &[]).unwrap();
        let header = extract_fits_header(&walked).unwrap();
        let fp = compute_header_fingerprint(&header);

        // Catalog rows under a NAME-PREFIX SIBLING root and a CASE-VARIANT root of
        // old root "/data/M31" — both must be invisible to the relink of /data/M31.
        let mut sibling_ids = Vec::new();
        for path in ["/data/M31_Ha/x.fits", "/data/m31/x.fits"] {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'x.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![path],
            ).unwrap();
            let id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, 'H', ?2)",
                rusqlite::params![id, fp],
            ).unwrap();
            sibling_ids.push((id, path.to_string()));
        }

        let res = relink_files(&conn, "/data/M31", new_root.to_str().unwrap()).unwrap();
        assert_eq!(
            res.files_matched, 0,
            "sibling fingerprints must not enter the map"
        );
        assert_eq!(res.files_new, 1);
        for (id, original) in &sibling_ids {
            let path: String = conn
                .query_row("SELECT path FROM files WHERE id = ?1", [id], |r| r.get(0))
                .unwrap();
            assert_eq!(&path, original, "sibling row must be untouched");
        }
    }

    #[test]
    fn relink_updates_path_and_filename_for_a_real_match() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let new_root = dir.path().join("relocated");
        std::fs::create_dir_all(&new_root).unwrap();
        let walked = new_root.join("renamed_on_disk.fits");
        crate::fits_writer::write_fits_f32(&walked, 4, 4, 1, &vec![0.0f32; 16], &[]).unwrap();
        let fp = compute_header_fingerprint(&extract_fits_header(&walked).unwrap());

        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format) VALUES ('/data/M31/orig.fits', 'orig.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
            [],
        ).unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, 'H', ?2)",
            rusqlite::params![id, fp],
        )
        .unwrap();

        let res = relink_files(&conn, "/data/M31", new_root.to_str().unwrap()).unwrap();
        assert_eq!(res.files_matched, 1);
        let (path, filename): (String, String) = conn
            .query_row(
                "SELECT path, filename FROM files WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(path, walked.to_string_lossy());
        assert_eq!(
            filename, "renamed_on_disk.fits",
            "filename must follow the path"
        );
    }
}
