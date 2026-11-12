//! Missing-files commands that read or reconcile the `missing_files` table:
//! the per-root listing and the recheck against the disk (and the zips), the
//! sync after a rescan, ignore / unignore and the purge of the catalog rows.
//! Both hosts wrap these; the table itself is kept by `db::sync_missing_files`.

use crate::api::{db, ApiError, PathPolicy};
use std::path::{Path, PathBuf};
use crate::services::ServiceContext;

/// Missing file record with status from the database
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MissingFileRecord {
    pub id: i64,
    pub file_id: i64,
    pub scan_root_id: i64,
    pub detected_at: String,
    pub last_checked_at: String,
    pub status: String, // "missing" or "ignored"
    // File info
    pub path: String,
    pub filename: String,
    pub size: i64,
    pub modified_at: String,
    // Frame info (if exists)
    pub has_frame: bool,
    pub object: Option<String>,
    pub date_obs: Option<String>,
}

/// Get missing files for a specific scan root with full details.
pub fn get_missing_files(
    ctx: &ServiceContext,
    root_id: i64,
) -> Result<Vec<MissingFileRecord>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let mut stmt = conn
        .prepare(
            "SELECT
                mf.id,
                mf.file_id,
                mf.scan_root_id,
                mf.detected_at,
                mf.last_checked_at,
                mf.status,
                f.path,
                f.filename,
                f.size,
                f.modified_at,
                CASE WHEN fr.id IS NOT NULL THEN 1 ELSE 0 END as has_frame,
                fr.object,
                fr.date_obs
             FROM missing_files mf
             JOIN files f ON f.id = mf.file_id
             LEFT JOIN frames fr ON fr.file_id = f.id
             WHERE mf.scan_root_id = ?1
             ORDER BY mf.detected_at DESC",
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let records = stmt
        .query_map([root_id], |row| {
            Ok(MissingFileRecord {
                id: row.get(0)?,
                file_id: row.get(1)?,
                scan_root_id: row.get(2)?,
                detected_at: row.get(3)?,
                last_checked_at: row.get(4)?,
                status: row.get(5)?,
                path: row.get(6)?,
                filename: row.get(7)?,
                size: row.get(8)?,
                modified_at: row.get(9)?,
                has_frame: row.get::<_, i32>(10)? == 1,
                object: row.get(11)?,
                date_obs: row.get(12)?,
            })
        })
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(records)
}

/// Recheck missing files for a scan root and update their status.
/// Returns the list of files that are still missing. No transaction: the
/// DELETE and the UPDATE run one by one on the catalog connection.
pub fn recheck_missing_files(
    ctx: &ServiceContext,
    root_id: i64,
) -> Result<Vec<MissingFileRecord>, ApiError> {
    // First phase: update the database
    {
        let db = db(ctx)?;
        let conn = db.conn();
        let now = chrono::Utc::now().to_rfc3339();

        // Get all missing files for this root. Also pull the zip columns: a row
        // inside a zip is found when its zip lists the entry, otherwise when
        // its file is on disk.
        let mut stmt = conn
            .prepare(
                "SELECT mf.id, mf.file_id, f.path, f.archive_zip_path, f.archive_path_in_zip
                 FROM missing_files mf
                 JOIN files f ON f.id = mf.file_id
                 WHERE mf.scan_root_id = ?1",
            )
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        let files: Vec<(i64, i64, String, Option<String>, Option<String>)> = stmt
            .query_map([root_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        // The same presence rule the scan-root check uses.
        let present =
            crate::zip_store::presence::present_flags(&files, |(_, _, path, zip, entry)| {
                (path.as_str(), zip.as_deref(), entry.as_deref())
            });
        let found_file_ids: Vec<i64> = files
            .iter()
            .zip(present)
            .filter(|(_, present)| *present)
            .map(|((mf_id, ..), _)| *mf_id)
            .collect();

        // Remove files that now exist
        if !found_file_ids.is_empty() {
            let placeholders: Vec<String> = found_file_ids.iter().map(|_| "?".to_string()).collect();
            let placeholders_str = placeholders.join(",");
            let delete_sql = format!(
                "DELETE FROM missing_files WHERE id IN ({})",
                placeholders_str
            );
            conn.execute(
                &delete_sql,
                rusqlite::params_from_iter(found_file_ids.iter()),
            )
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        }

        // Update last_checked_at for remaining files
        conn.execute(
            "UPDATE missing_files SET last_checked_at = ?1 WHERE scan_root_id = ?2",
            rusqlite::params![&now, root_id],
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    }

    // Second phase: return the updated list
    get_missing_files(ctx, root_id)
}

/// Sync the `missing_files` table to the file ids a rescan found missing:
/// inserts new entries, removes entries for files that now exist, keeps the
/// 'ignored' ones.
pub fn sync_missing_files(
    ctx: &ServiceContext,
    root_id: i64,
    file_ids: &[i64],
) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    crate::db::sync_missing_files(&conn, root_id, file_ids)
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// Mark a missing file as ignored.
pub fn ignore_missing_file(ctx: &ServiceContext, file_id: i64) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    conn.execute(
        "UPDATE missing_files SET status = 'ignored' WHERE file_id = ?1",
        [file_id],
    )
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(())
}

/// Remove the ignored status from a missing file.
pub fn unignore_missing_file(ctx: &ServiceContext, file_id: i64) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    conn.execute(
        "UPDATE missing_files SET status = 'missing' WHERE file_id = ?1",
        [file_id],
    )
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(())
}

/// Delete files from the database entirely (removes from files, frames,
/// missing_files tables). An empty list is a no-op.
pub fn delete_missing_files(ctx: &ServiceContext, file_ids: &[i64]) -> Result<(), ApiError> {
    if file_ids.is_empty() {
        return Ok(());
    }

    let db = db(ctx)?;
    let conn = db.conn();

    // Not a bare `DELETE FROM files`: a master's file needs its raw source set
    // un-superseded and its consumers repointed first, or the raw frames stay
    // invisible to the matcher with nothing left in the UI to undo it
    // (2026-08-02 audit C3). `delete_orphaned_files` is the one path that does
    // both — the Black Hole, void and orphan-purge flows all go through it.
    // Pinned by `deleting_a_master_file_un_supersedes_its_raw_set` below.
    crate::relinking::delete_orphaned_files(&conn, file_ids)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(())
}

/// Point a missing file at a new location: updates `files.path` and
/// `files.filename`, then drops the `missing_files` row.
///
/// The path is checked against `policy` BEFORE the disk is asked whether it
/// exists, so a path outside the allowed roots is refused without saying whether
/// it exists. Per `PathPolicy::check`'s precondition the candidate is
/// canonicalized for the check (a path that cannot be canonicalized is checked as
/// given); the catalog stores the user's own spelling (`normalize_separators`),
/// never the canonical one. Desktop passes `AllowAll`.
pub fn relocate_missing_file(
    ctx: &ServiceContext,
    file_id: i64,
    new_path: String,
    policy: &PathPolicy,
) -> Result<(), ApiError> {
    let new_path = crate::db::normalize_separators(&new_path);

    let candidate = Path::new(&new_path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&new_path));
    policy.check(&candidate)?;

    // Verify the new path exists
    if !Path::new(&new_path).exists() {
        return Err(ApiError::Invalid(format!(
            "File does not exist at path: {}",
            new_path
        )));
    }

    let db = db(ctx)?;
    let conn = db.conn();

    // Extract new filename from path
    let new_filename = Path::new(&new_path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| new_path.clone());

    // Update the file path
    conn.execute(
        "UPDATE files SET path = ?1, filename = ?2 WHERE id = ?3",
        rusqlite::params![&new_path, &new_filename, file_id],
    )
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Remove from missing_files table
    conn.execute("DELETE FROM missing_files WHERE file_id = ?1", [file_id])
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real-`Database` [`ServiceContext`] over a tempdir catalog.
    fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        (tmp, ctx)
    }

    /// A file that is back on disk is dropped from `missing_files`; one that is
    /// still absent stays, with `last_checked_at` refreshed.
    #[test]
    fn recheck_drops_files_back_on_disk_and_keeps_absent_ones() {
        let (tmp, ctx) = test_ctx();
        let back = tmp.path().join("back.fits");
        std::fs::write(&back, b"x").unwrap();
        let gone = tmp.path().join("gone.fits");
        {
            let conn = ctx.db.get().unwrap().conn();
            conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [tmp.path().to_str().unwrap()])
                .unwrap();
            for (id, p, name) in [(1, &back, "back.fits"), (2, &gone, "gone.fits")] {
                conn.execute(
                    "INSERT INTO files (id, path, filename, size, modified_at, format)
                     VALUES (?1, ?2, ?3, 1, '2025-01-01T00:00:00Z', 'FITS')",
                    rusqlite::params![id, p.to_str().unwrap(), name],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO missing_files (file_id, scan_root_id, detected_at, last_checked_at)
                     VALUES (?1, 1, '2025-01-02T00:00:00Z', '2025-01-02T00:00:00Z')",
                    [id],
                )
                .unwrap();
            }
        }

        let still_missing = recheck_missing_files(&ctx, 1).expect("recheck");

        assert_eq!(still_missing.len(), 1);
        assert_eq!(still_missing[0].file_id, 2);
        assert_eq!(still_missing[0].status, "missing");
        assert_ne!(still_missing[0].last_checked_at, "2025-01-02T00:00:00Z");
    }

    /// Ignore and unignore flip the status of the row; the purge of an empty
    /// list is a no-op.
    #[test]
    fn ignore_and_unignore_flip_the_status() {
        let (tmp, ctx) = test_ctx();
        {
            let conn = ctx.db.get().unwrap().conn();
            conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [tmp.path().to_str().unwrap()])
                .unwrap();
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (1, '/x/a.fits', 'a.fits', 1, '2025-01-01T00:00:00Z', 'FITS')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO missing_files (file_id, scan_root_id, detected_at, last_checked_at)
                 VALUES (1, 1, '2025-01-02T00:00:00Z', '2025-01-02T00:00:00Z')",
                [],
            )
            .unwrap();
        }
        let status = || -> String {
            ctx.db
                .get()
                .unwrap()
                .conn()
                .query_row("SELECT status FROM missing_files WHERE file_id = 1", [], |r| r.get(0))
                .unwrap()
        };
        ignore_missing_file(&ctx, 1).unwrap();
        assert_eq!(status(), "ignored");
        unignore_missing_file(&ctx, 1).unwrap();
        assert_eq!(status(), "missing");
        delete_missing_files(&ctx, &[]).unwrap();
        assert_eq!(status(), "missing");
    }

    /// Purging a MASTER's file through the missing-files panel must run the
    /// same un-supersede the orphan-purge path does (2026-08-02 audit C3):
    /// otherwise the raw source set keeps pointing at a master that exists
    /// neither on disk nor in the catalog, its frames stay invisible to the
    /// matcher forever, and nothing in the UI can undo it.
    #[test]
    fn deleting_a_master_file_un_supersedes_its_raw_set() {
        let (_tmp, ctx) = test_ctx();

        let (file_id, raw_set_id) = {
            let conn = ctx.db.get().unwrap().conn();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES ('/lib/master_dark.fits', 'master_dark.fits', 100, '2026-09-01T00:00:00Z', 'FITS')",
                [],
            )
            .unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp, is_master) VALUES (?1, 'MasterDark', 1)",
                rusqlite::params![file_id],
            )
            .unwrap();
            let frame_id = conn.last_insert_rowid();

            conn.execute(
                "INSERT INTO calibration_set (imagetyp, date, is_master_library)
                 VALUES ('MasterDark', '2026-09-01', 1)",
                [],
            )
            .unwrap();
            let master_set_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![master_set_id, frame_id],
            )
            .unwrap();

            conn.execute(
                "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-09-01')",
                [],
            )
            .unwrap();
            let raw_set_id = conn.last_insert_rowid();
            conn.execute(
                "UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2",
                rusqlite::params![master_set_id, raw_set_id],
            )
            .unwrap();

            (file_id, raw_set_id)
        };

        delete_missing_files(&ctx, &[file_id]).expect("delete should succeed");

        let db = ctx.db.get().unwrap();
        let conn = db.conn();
        let still_superseded: Option<i64> = conn
            .query_row(
                "SELECT superseded_by_set_id FROM calibration_set WHERE id = ?1",
                rusqlite::params![raw_set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            still_superseded, None,
            "raw set must be un-superseded when its master's file is purged"
        );

        let files_left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE id = ?1",
                rusqlite::params![file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(files_left, 0, "the purged file row is gone");
    }

    /// One missing file (id 1) under scan root 1, recorded at `old_path`.
    fn seed_missing(ctx: &ServiceContext, root: &Path, old_path: &str) {
        let conn = ctx.db.get().unwrap().conn();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [root.to_str().unwrap()])
            .unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1, ?1, 'old.fits', 1, '2025-01-01T00:00:00Z', 'FITS')",
            [old_path],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO missing_files (file_id, scan_root_id, detected_at, last_checked_at)
             VALUES (1, 1, '2025-01-02T00:00:00Z', '2025-01-02T00:00:00Z')",
            [],
        )
        .unwrap();
    }

    fn catalog_state(ctx: &ServiceContext) -> (String, String, i64) {
        let conn = ctx.db.get().unwrap().conn();
        let (path, name): (String, String) = conn
            .query_row("SELECT path, filename FROM files WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM missing_files WHERE file_id = 1", [], |r| r.get(0))
            .unwrap();
        (path, name, rows)
    }

    #[test]
    fn relocate_missing_file_updates_the_row_and_drops_the_missing_entry() {
        let (tmp, ctx) = test_ctx();
        seed_missing(&ctx, tmp.path(), "/gone/old.fits");
        let new = tmp.path().join("moved-1.fits");
        std::fs::write(&new, b"x").unwrap();
        let new_str = new.to_str().unwrap().to_string();

        relocate_missing_file(&ctx, 1, new_str.clone(), &PathPolicy::AllowAll).expect("relocate");

        let (path, name, rows) = catalog_state(&ctx);
        assert_eq!(path, crate::db::normalize_separators(&new_str));
        assert_eq!(name, "moved-1.fits");
        assert_eq!(rows, 0);
    }

    #[test]
    fn relocate_missing_file_to_a_path_that_does_not_exist_is_refused() {
        let (tmp, ctx) = test_ctx();
        seed_missing(&ctx, tmp.path(), "/gone/old.fits");
        let nope = tmp.path().join("nope.fits");
        let nope_str = nope.to_str().unwrap().to_string();

        match relocate_missing_file(&ctx, 1, nope_str.clone(), &PathPolicy::AllowAll) {
            Err(ApiError::Invalid(m)) => assert_eq!(
                m,
                format!(
                    "File does not exist at path: {}",
                    crate::db::normalize_separators(&nope_str)
                )
            ),
            other => panic!("expected Invalid, got {other:?}"),
        }
        let (path, _, rows) = catalog_state(&ctx);
        assert_eq!(path, "/gone/old.fits");
        assert_eq!(rows, 1);
    }

    /// A path outside the allowed roots is refused the same way whether it
    /// exists or not, and the catalog is untouched.
    #[test]
    fn relocate_missing_file_outside_the_allowed_roots_is_refused_without_leaking_existence() {
        let (tmp, ctx) = test_ctx();
        let allowed = tmp.path().join("allowed");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        seed_missing(&ctx, &allowed, "/gone/old.fits");
        let existing = outside.join("there-1.fits");
        std::fs::write(&existing, b"x").unwrap();
        let absent = outside.join("absent-1.fits");
        let policy = PathPolicy::AllowedRoots(vec![crate::test_support::canonical_path(&allowed)]);

        for p in [&existing, &absent] {
            match relocate_missing_file(&ctx, 1, p.to_str().unwrap().to_string(), &policy) {
                Err(ApiError::Forbidden(m)) => assert!(m.contains("outside the allowed roots"), "{m}"),
                other => panic!("expected Forbidden, got {other:?}"),
            }
        }
        // `..` back out of the allowed root is resolved before the check.
        let sneaky = allowed.join("..").join("outside").join("there-1.fits");
        assert!(matches!(
            relocate_missing_file(&ctx, 1, sneaky.to_str().unwrap().to_string(), &policy),
            Err(ApiError::Forbidden(_))
        ));
        let (path, _, rows) = catalog_state(&ctx);
        assert_eq!(path, "/gone/old.fits");
        assert_eq!(rows, 1);

        // Inside the allowed root it goes through.
        let inside = allowed.join("here-1.fits");
        std::fs::write(&inside, b"x").unwrap();
        relocate_missing_file(&ctx, 1, inside.to_str().unwrap().to_string(), &policy).expect("inside");
        assert_eq!(catalog_state(&ctx).2, 0);
    }
}
