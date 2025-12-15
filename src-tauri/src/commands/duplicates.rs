// Duplicate detection and black hole management commands

use crate::db::{self};
use tauri::State;

use super::AppState;

/// Update the allow_duplicates flag for a scan root
#[tauri::command]
pub async fn set_scan_root_duplicates_flag(
    id: i64,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::update_scan_root_duplicates_flag(&conn, id, enabled).map_err(|e| e.to_string())
}

/// Move a file to the black hole (soft delete)
#[tauri::command]
pub async fn move_to_black_hole(
    file_id: i64,
    from_where: String,
    state: State<'_, AppState>,
) -> Result<i64, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get original path
    let original_path: String = conn
        .query_row("SELECT path FROM files WHERE id = ?1", [file_id], |row| {
            row.get(0)
        })
        .map_err(|e| e.to_string())?;

    db::add_to_black_hole(&conn, file_id, &from_where, &original_path).map_err(|e| e.to_string())
}

/// Get all files in the black hole
#[tauri::command]
pub async fn get_black_hole_files(
    filter: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::BlackHoleEntry>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_black_hole_files(&conn, filter).map_err(|e| e.to_string())
}

/// Restore a file from the black hole
#[tauri::command]
pub async fn restore_from_black_hole(
    file_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::remove_from_black_hole(&conn, file_id).map_err(|e| e.to_string())
}

/// Permanently delete a file (send to void)
#[tauri::command]
pub async fn send_to_void(file_id: i64, state: State<'_, AppState>) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::send_to_void(&conn, file_id).map_err(|e| e.to_string())
}

/// Permanently delete all files in black hole (send all to void)
#[tauri::command]
pub async fn send_all_to_void(state: State<'_, AppState>) -> Result<usize, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::send_all_to_void(&conn).map_err(|e| e.to_string())
}

/// Get folders with high duplicate file similarity
#[tauri::command]
pub async fn get_duplicate_folders(
    threshold: Option<f64>,
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::FolderSimilarity>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let similarity_threshold = threshold.unwrap_or(70.0);
    db::find_duplicate_folders(&conn, similarity_threshold).map_err(|e| e.to_string())
}

/// Backfill fingerprints for existing FITS headers
#[tauri::command]
pub async fn backfill_header_fingerprints(
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get all headers without fingerprints
    let mut stmt = conn
        .prepare("SELECT id, header FROM fits_header WHERE header_fingerprint IS NULL")
        .map_err(|e| e.to_string())?;

    let headers: Vec<(i64, String)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let total = headers.len();
    println!("Backfilling fingerprints for {} headers", total);

    // Compute and update fingerprints
    for (id, header) in headers {
        let fingerprint = crate::fingerprint::compute_header_fingerprint(&header);
        conn.execute(
            "UPDATE fits_header SET header_fingerprint = ?1 WHERE id = ?2",
            rusqlite::params![fingerprint, id],
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(total)
}
