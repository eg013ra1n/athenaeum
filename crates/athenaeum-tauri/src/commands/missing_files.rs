use std::collections::HashMap;
use std::path::Path;
use tauri::State;

use crate::commands::AppState;

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

/// Sync missing files to the database after a rescan
/// This updates the missing_files table with the current state
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn sync_missing_files(
    root_id: i64,
    file_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    athenaeum_core::db::sync_missing_files(&conn, root_id, &file_ids).map_err(|e| e.to_string())
}

/// Get missing files for a specific scan root with full details
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_missing_files(
    root_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<MissingFileRecord>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
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
        .map_err(|e| e.to_string())?;

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
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(records)
}

/// Get missing files counts for all scan roots (for indicators)
/// Returns a map of scan_root_id -> count (only 'missing' status, not 'ignored')
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_missing_files_counts(
    state: State<'_, AppState>,
) -> Result<HashMap<i64, i64>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let mut stmt = conn
        .prepare(
            "SELECT scan_root_id, COUNT(*)
             FROM missing_files
             WHERE status = 'missing'
             GROUP BY scan_root_id",
        )
        .map_err(|e| e.to_string())?;

    let mut counts = HashMap::new();
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
        .map_err(|e| e.to_string())?;

    for row in rows {
        let (root_id, count) = row.map_err(|e| e.to_string())?;
        counts.insert(root_id, count);
    }

    Ok(counts)
}

/// Recheck missing files for a scan root and update their status
/// Returns the list of files that are still missing
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn recheck_missing_files(
    root_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<MissingFileRecord>, String> {
    // First phase: update the database
    {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
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
            .map_err(|e| e.to_string())?;

        let files: Vec<(i64, i64, String, Option<String>, Option<String>)> = stmt
            .query_map([root_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        // The same presence rule the web host and the scan-root check use.
        let present =
            athenaeum_core::zip_store::presence::present_flags(&files, |(_, _, path, zip, entry)| {
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
            .map_err(|e| e.to_string())?;
        }

        // Update last_checked_at for remaining files
        conn.execute(
            "UPDATE missing_files SET last_checked_at = ?1 WHERE scan_root_id = ?2",
            rusqlite::params![&now, root_id],
        )
        .map_err(|e| e.to_string())?;
    }

    // Second phase: return the updated list
    get_missing_files(root_id, state).await
}

/// Mark a missing file as ignored
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn ignore_missing_file(
    file_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    conn.execute(
        "UPDATE missing_files SET status = 'ignored' WHERE file_id = ?1",
        [file_id],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Remove ignored status from a missing file
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn unignore_missing_file(
    file_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    conn.execute(
        "UPDATE missing_files SET status = 'missing' WHERE file_id = ?1",
        [file_id],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Delete files from the database entirely (removes from files, frames, missing_files tables)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_missing_files(
    file_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if file_ids.is_empty() {
        return Ok(());
    }

    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Not a bare `DELETE FROM files`: a master's file needs its raw source set
    // un-superseded and its consumers repointed first, or the raw frames stay
    // invisible to the matcher with nothing left in the UI to undo it
    // (2026-08-02 audit C3). `delete_orphaned_files` is the one path that does
    // both — the Black Hole, void and orphan-purge flows all go through it.
    // Pinned by `deleting_a_master_file_un_supersedes_its_raw_set` on the web
    // mirror of this command.
    athenaeum_core::relinking::delete_orphaned_files(&conn, &file_ids)
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Relocate a missing file to a new path
/// Updates the file path in the database and removes from missing_files
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn relocate_missing_file(
    file_id: i64,
    new_path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let new_path = athenaeum_core::db::normalize_separators(&new_path);
    // Verify the new path exists
    if !Path::new(&new_path).exists() {
        return Err(format!("File does not exist at path: {}", new_path));
    }

    let db = state.ctx.db.get().ok_or("Database not initialized")?;
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
    .map_err(|e| e.to_string())?;

    // Remove from missing_files table
    conn.execute("DELETE FROM missing_files WHERE file_id = ?1", [file_id])
        .map_err(|e| e.to_string())?;

    Ok(())
}
