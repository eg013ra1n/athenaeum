// Missing files route handlers — mirrors athenaeum-tauri/src/commands/missing_files.rs

use axum::{extract::State, http::StatusCode, Json};
use rayon::prelude::*;
use std::path::Path;

use crate::WebAppState;

// ── Error helpers ─────────────────────────────────────────────────────────────

fn db_err(msg: impl std::fmt::Display) -> (StatusCode, String) {
    let s = msg.to_string();
    eprintln!("missing_files error: {}", s);
    (StatusCode::INTERNAL_SERVER_ERROR, s)
}

fn no_db() -> (StatusCode, String) {
    eprintln!("missing_files error: database not initialized");
    (StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string())
}

// ── Response DTOs ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct MissingFileRecord {
    pub id: i64,
    pub file_id: i64,
    pub scan_root_id: i64,
    pub detected_at: String,
    pub last_checked_at: String,
    pub status: String,
    pub path: String,
    pub filename: String,
    pub size: i64,
    pub modified_at: String,
    pub has_frame: bool,
    pub object: Option<String>,
    pub date_obs: Option<String>,
}

#[derive(serde::Serialize)]
pub struct OrphanedFile {
    pub id: i64,
    pub path: String,
    pub filename: String,
    pub size: i64,
    pub modified_at: String,
    pub has_frame: bool,
    pub object: Option<String>,
    pub date_obs: Option<String>,
}

// ── Request body types ────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootIdArgs {
    pub root_id: i64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncMissingFilesArgs {
    pub root_id: i64,
    pub file_ids: Vec<i64>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIdArgs {
    pub file_id: i64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteMissingFilesArgs {
    pub file_ids: Vec<i64>,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// POST /api/check_missing_files_in_scan_root
///
/// Check for files in the DB under this root that no longer exist on disk.
/// Emits SSE progress events during the filesystem check.
pub async fn check_missing_files_in_scan_root(
    State(state): State<WebAppState>,
    Json(args): Json<RootIdArgs>,
) -> Result<Json<Vec<OrphanedFile>>, (StatusCode, String)> {
    let root_id = args.root_id;

    // Collect files from database
    let files = {
        let db = state.ctx.db.get().ok_or_else(no_db)?;
        let conn = db.conn();

        let path: String = conn
            .query_row(
                "SELECT path FROM scan_roots WHERE id = ?1",
                rusqlite::params![root_id],
                |row| row.get(0),
            )
            .map_err(|e| db_err(format!("Failed to get scan root: {}", e)))?;

        // Exclude files known to be inside an archive zip — their on-disk
        // paths intentionally don't exist post-archive, so the existence
        // check below would falsely flag them as missing.
        let mut stmt = conn
            .prepare(
                "SELECT f.id, f.path, f.filename, f.size, f.modified_at,
                        CASE WHEN fr.id IS NOT NULL THEN 1 ELSE 0 END as has_frame,
                        fr.object, fr.date_obs
                 FROM files f
                 LEFT JOIN frames fr ON fr.file_id = f.id
                 WHERE f.path LIKE ?1 AND f.archived_in_operation IS NULL",
            )
            .map_err(db_err)?;

        let path_prefix = format!("{}%", path);
        let rows = stmt.query_map(rusqlite::params![path_prefix], |row| {
            Ok(OrphanedFile {
                id: row.get(0)?,
                path: row.get(1)?,
                filename: row.get(2)?,
                size: row.get(3)?,
                modified_at: row.get(4)?,
                has_frame: row.get::<_, i64>(5)? != 0,
                object: row.get(6).ok(),
                date_obs: row.get(7).ok(),
            })
        })
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
        rows
    };

    // Filter to only files that don't exist on disk — done in parallel outside the lock
    let missing_files: Vec<OrphanedFile> = files
        .into_par_iter()
        .filter(|file| !Path::new(&file.path).exists())
        .collect();

    Ok(Json(missing_files))
}

/// POST /api/sync_missing_files
///
/// Sync the missing_files table after a rescan. Inserts new missing entries,
/// removes entries for files that now exist, preserves 'ignored' entries.
pub async fn sync_missing_files(
    State(state): State<WebAppState>,
    Json(args): Json<SyncMissingFilesArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute("BEGIN TRANSACTION", []).map_err(db_err)?;

    if args.file_ids.is_empty() {
        conn.execute(
            "DELETE FROM missing_files WHERE scan_root_id = ?1 AND status = 'missing'",
            [args.root_id],
        )
        .map_err(db_err)?;
    } else {
        let placeholders: Vec<String> = args.file_ids.iter().map(|_| "?".to_string()).collect();
        let delete_sql = format!(
            "DELETE FROM missing_files WHERE scan_root_id = ?1 AND status = 'missing' AND file_id NOT IN ({})",
            placeholders.join(",")
        );
        let mut params: Vec<rusqlite::types::Value> = vec![args.root_id.into()];
        for id in &args.file_ids {
            params.push((*id).into());
        }
        conn.execute(&delete_sql, rusqlite::params_from_iter(params))
            .map_err(db_err)?;
    }

    for file_id in &args.file_ids {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM missing_files WHERE file_id = ?1",
                [file_id],
                |_| Ok(true),
            )
            .unwrap_or(false);

        if exists {
            conn.execute(
                "UPDATE missing_files SET last_checked_at = ?1 WHERE file_id = ?2",
                rusqlite::params![&now, file_id],
            )
            .map_err(db_err)?;
        } else {
            conn.execute(
                "INSERT INTO missing_files (file_id, scan_root_id, detected_at, last_checked_at, status)
                 VALUES (?1, ?2, ?3, ?4, 'missing')",
                rusqlite::params![file_id, args.root_id, &now, &now],
            )
            .map_err(db_err)?;
        }
    }

    conn.execute("COMMIT", []).map_err(db_err)?;
    Ok(Json(()))
}

/// POST /api/get_missing_files
///
/// Return all missing file records for a scan root with full details.
pub async fn get_missing_files(
    State(state): State<WebAppState>,
    Json(args): Json<RootIdArgs>,
) -> Result<Json<Vec<MissingFileRecord>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    get_missing_files_internal(&conn, args.root_id)
}

/// POST /api/recheck_missing_files
///
/// Re-verify all missing files for a root on disk, removing entries for files
/// that have reappeared, then return the updated list.
pub async fn recheck_missing_files(
    State(state): State<WebAppState>,
    Json(args): Json<RootIdArgs>,
) -> Result<Json<Vec<MissingFileRecord>>, (StatusCode, String)> {
    let root_id = args.root_id;

    {
        let db = state.ctx.db.get().ok_or_else(no_db)?;
        let conn = db.conn();
        let now = chrono::Utc::now().to_rfc3339();

        // Pull `archived_in_operation` alongside the path so we can drop
        // rows for files that have since been moved into an archive zip.
        let mut stmt = conn
            .prepare(
                "SELECT mf.id, mf.file_id, f.path, f.archived_in_operation
                 FROM missing_files mf
                 JOIN files f ON f.id = mf.file_id
                 WHERE mf.scan_root_id = ?1",
            )
            .map_err(db_err)?;

        let files: Vec<(i64, i64, String, Option<i64>)> = stmt
            .query_map([root_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
            .map_err(db_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err)?;

        // "Found" means archived (catalog says it's in a zip) OR present on disk.
        let found_mf_ids: Vec<i64> = files
            .par_iter()
            .filter_map(|(mf_id, _file_id, path, archived_op)| {
                if archived_op.is_some() || Path::new(path).exists() {
                    Some(*mf_id)
                } else {
                    None
                }
            })
            .collect();

        if !found_mf_ids.is_empty() {
            let placeholders: Vec<String> = found_mf_ids.iter().map(|_| "?".to_string()).collect();
            let delete_sql = format!(
                "DELETE FROM missing_files WHERE id IN ({})",
                placeholders.join(",")
            );
            conn.execute(
                &delete_sql,
                rusqlite::params_from_iter(found_mf_ids.iter()),
            )
            .map_err(db_err)?;
        }

        conn.execute(
            "UPDATE missing_files SET last_checked_at = ?1 WHERE scan_root_id = ?2",
            rusqlite::params![&now, root_id],
        )
        .map_err(db_err)?;
    }

    // Return updated list
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();
    get_missing_files_internal(&conn, root_id)
}

/// POST /api/ignore_missing_file
pub async fn ignore_missing_file(
    State(state): State<WebAppState>,
    Json(args): Json<FileIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    conn.execute(
        "UPDATE missing_files SET status = 'ignored' WHERE file_id = ?1",
        [args.file_id],
    )
    .map_err(db_err)?;

    Ok(Json(()))
}

/// POST /api/unignore_missing_file
pub async fn unignore_missing_file(
    State(state): State<WebAppState>,
    Json(args): Json<FileIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    conn.execute(
        "UPDATE missing_files SET status = 'missing' WHERE file_id = ?1",
        [args.file_id],
    )
    .map_err(db_err)?;

    Ok(Json(()))
}

/// POST /api/delete_missing_files
///
/// Delete files from the database entirely (CASCADE handles frames, missing_files, etc.)
pub async fn delete_missing_files(
    State(state): State<WebAppState>,
    Json(args): Json<DeleteMissingFilesArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    if args.file_ids.is_empty() {
        return Ok(Json(()));
    }

    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let placeholders: Vec<String> = args.file_ids.iter().map(|_| "?".to_string()).collect();
    let delete_sql = format!("DELETE FROM files WHERE id IN ({})", placeholders.join(","));
    conn.execute(&delete_sql, rusqlite::params_from_iter(args.file_ids.iter()))
        .map_err(db_err)?;

    Ok(Json(()))
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn get_missing_files_internal(
    conn: &rusqlite::Connection,
    root_id: i64,
) -> Result<Json<Vec<MissingFileRecord>>, (StatusCode, String)> {
    let mut stmt = conn
        .prepare(
            "SELECT
                mf.id, mf.file_id, mf.scan_root_id,
                mf.detected_at, mf.last_checked_at, mf.status,
                f.path, f.filename, f.size, f.modified_at,
                CASE WHEN fr.id IS NOT NULL THEN 1 ELSE 0 END as has_frame,
                fr.object, fr.date_obs
             FROM missing_files mf
             JOIN files f ON f.id = mf.file_id
             LEFT JOIN frames fr ON fr.file_id = f.id
             WHERE mf.scan_root_id = ?1
             ORDER BY mf.detected_at DESC",
        )
        .map_err(db_err)?;

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
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;

    Ok(Json(records))
}
