use std::collections::HashMap;
use std::path::Path;
use tauri::State;
use rayon::prelude::*;

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
pub async fn sync_missing_files(
    root_id: i64,
    file_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();
    let now = chrono::Utc::now().to_rfc3339();

    // Start a transaction
    conn.execute("BEGIN TRANSACTION", [])
        .map_err(|e| e.to_string())?;

    // Remove any files that are no longer missing (they exist on disk now)
    // by deleting records where file_id is NOT in the new missing list
    // and status is 'missing' (not 'ignored')
    if file_ids.is_empty() {
        // All files are present - remove all 'missing' status entries for this root
        conn.execute(
            "DELETE FROM missing_files WHERE scan_root_id = ?1 AND status = 'missing'",
            [root_id],
        )
        .map_err(|e| e.to_string())?;
    } else {
        // Build placeholders for the IN clause
        let placeholders: Vec<String> = file_ids.iter().map(|_| "?".to_string()).collect();
        let placeholders_str = placeholders.join(",");

        // Delete records for files that are no longer missing (but keep 'ignored' ones)
        let delete_sql = format!(
            "DELETE FROM missing_files WHERE scan_root_id = ?1 AND status = 'missing' AND file_id NOT IN ({})",
            placeholders_str
        );
        let mut params: Vec<rusqlite::types::Value> = vec![root_id.into()];
        for id in &file_ids {
            params.push((*id).into());
        }
        conn.execute(&delete_sql, rusqlite::params_from_iter(params))
            .map_err(|e| e.to_string())?;
    }

    // Insert or update missing files
    for file_id in &file_ids {
        // Check if already exists
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM missing_files WHERE file_id = ?1",
                [file_id],
                |_| Ok(true),
            )
            .unwrap_or(false);

        if exists {
            // Update last_checked_at, but don't change status if it's 'ignored'
            conn.execute(
                "UPDATE missing_files SET last_checked_at = ?1 WHERE file_id = ?2",
                rusqlite::params![&now, file_id],
            )
            .map_err(|e| e.to_string())?;
        } else {
            // Insert new missing file record
            conn.execute(
                "INSERT INTO missing_files (file_id, scan_root_id, detected_at, last_checked_at, status)
                 VALUES (?1, ?2, ?3, ?4, 'missing')",
                rusqlite::params![file_id, root_id, &now, &now],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    conn.execute("COMMIT", []).map_err(|e| e.to_string())?;
    Ok(())
}

/// Get missing files for a specific scan root with full details
#[tauri::command]
pub async fn get_missing_files(
    root_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<MissingFileRecord>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
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
pub async fn get_missing_files_counts(
    state: State<'_, AppState>,
) -> Result<HashMap<i64, i64>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
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
pub async fn recheck_missing_files(
    root_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<MissingFileRecord>, String> {
    // First phase: update the database
    {
        let state_lock = state.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();
        let now = chrono::Utc::now().to_rfc3339();

        // Get all missing files for this root
        let mut stmt = conn
            .prepare(
                "SELECT mf.id, mf.file_id, f.path
                 FROM missing_files mf
                 JOIN files f ON f.id = mf.file_id
                 WHERE mf.scan_root_id = ?1",
            )
            .map_err(|e| e.to_string())?;

        let files: Vec<(i64, i64, String)> = stmt
            .query_map([root_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        // Check each file in parallel - filesystem checks are I/O bound
        let found_file_ids: Vec<i64> = files
            .par_iter()
            .filter_map(|(mf_id, _file_id, path)| {
                if Path::new(path).exists() {
                    Some(*mf_id)
                } else {
                    None
                }
            })
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
    } // state_lock is dropped here

    // Second phase: return the updated list
    get_missing_files(root_id, state).await
}

/// Mark a missing file as ignored
#[tauri::command]
pub async fn ignore_missing_file(
    file_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
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
pub async fn unignore_missing_file(
    file_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
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
pub async fn delete_missing_files(
    file_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if file_ids.is_empty() {
        return Ok(());
    }

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let placeholders: Vec<String> = file_ids.iter().map(|_| "?".to_string()).collect();
    let placeholders_str = placeholders.join(",");

    // Delete from files table (CASCADE will handle frames, missing_files, etc.)
    let delete_sql = format!("DELETE FROM files WHERE id IN ({})", placeholders_str);
    conn.execute(&delete_sql, rusqlite::params_from_iter(file_ids.iter()))
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Relocate a missing file to a new path
/// Updates the file path in the database and removes from missing_files
#[tauri::command]
pub async fn relocate_missing_file(
    file_id: i64,
    new_path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Verify the new path exists
    if !Path::new(&new_path).exists() {
        return Err(format!("File does not exist at path: {}", new_path));
    }

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
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
