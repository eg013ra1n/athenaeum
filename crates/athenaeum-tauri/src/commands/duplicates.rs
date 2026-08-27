// Duplicate detection and black hole management commands

use crate::db::{self};
use tauri::{Emitter, State};

use super::AppState;

/// Update the allow_duplicates flag for a scan root
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_scan_root_duplicates_flag(
    id: i64,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::update_scan_root_duplicates_flag(&conn, id, enabled).map_err(|e| e.to_string())
}

/// Move a batch of files to the black hole in a single transaction, emitting
/// progress events as it runs. Used by the duplicates batch-deletion UI.
///
/// Returns `BulkMoveResult { moved, failed }`. Per-file failures don't abort
/// the batch — they're logged and reported in `failed`.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn bulk_move_to_black_hole(
    file_ids: Vec<i64>,
    from_where: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<crate::models::BulkMoveResult, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let emitter = crate::tauri_events::TauriProgressEmitter(app_handle.clone());
    let result = db::bulk_move_to_black_hole(&conn, &file_ids, &from_where, Some(&emitter))
        .map_err(|e| e.to_string())?;

    // Fire a single `blackhole-changed` event so other views (Black Hole
    // tab, file manager, missing-metadata) invalidate their caches. The
    // payload intentionally has no file_id — consumers should just refresh.
    let _ = app_handle.emit("blackhole-changed", serde_json::json!({
        "action": "bulk-blackholed",
        "count": result.moved,
    }));

    Ok(result)
}

/// Move a file to the black hole (soft delete)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn move_to_black_hole(
    file_id: i64,
    from_where: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<i64, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get original path
    let original_path: String = conn
        .query_row("SELECT path FROM files WHERE id = ?1", [file_id], |row| {
            row.get(0)
        })
        .map_err(|e| e.to_string())?;

    let id = db::add_to_black_hole(&conn, file_id, &from_where, &original_path).map_err(|e| e.to_string())?;

    let _ = app_handle.emit("blackhole-changed", serde_json::json!({
        "file_id": file_id,
        "action": "blackholed"
    }));

    Ok(id)
}

/// Get all files in the black hole
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_black_hole_files(
    filter: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::BlackHoleEntry>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_black_hole_files(&conn, filter).map_err(|e| e.to_string())
}

/// Check which file IDs from a given list are in the black hole
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_blackholed_file_ids(
    file_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<Vec<i64>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_blackholed_file_ids(&conn, &file_ids).map_err(|e| e.to_string())
}

/// Restore a file from the black hole
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn restore_from_black_hole(
    file_id: i64,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::remove_from_black_hole(&conn, file_id).map_err(|e| e.to_string())?;

    let _ = app_handle.emit("blackhole-changed", serde_json::json!({
        "file_id": file_id,
        "action": "restored"
    }));

    Ok(())
}

/// Permanently delete a file (send to void)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn send_to_void(file_id: i64, state: State<'_, AppState>) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::send_to_void(&conn, file_id).map_err(|e| e.to_string())
}

/// Permanently delete all files in black hole (send all to void)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn send_all_to_void(state: State<'_, AppState>) -> Result<usize, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::send_all_to_void(&conn).map_err(|e| e.to_string())
}

/// Get folders with high duplicate file similarity
/// Always computes fresh since folder similarity depends on current file state
/// and can't easily filter out black_hole files from cached data
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_duplicate_folders(
    threshold: Option<f64>,
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::FolderSimilarity>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let similarity_threshold = threshold.unwrap_or(70.0);

    // Always compute fresh - folder similarity depends on current file state
    // and the cache can't account for files moved to black_hole
    db::find_duplicate_folders(&conn, similarity_threshold).map_err(|e| e.to_string())
}

/// Deep-verify two files are byte-identical. Use as an opt-in safety net
/// before destructive operations on duplicates — `compute_xxhash` only
/// samples 3×512 KiB regions of a file, so two genuinely different large
/// files can collide in the sampled hash.
/// Deep-verify two CATALOG files, banking the read: an identical pair's
/// full-content hash lands in `files.strong_hash`, and a later verify of the
/// same pair is decided from the stored hashes without reading either file.
/// Fired per-file by the Duplicates view's verify loop — debug-level span.
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
pub async fn verify_duplicate_pair(
    file_a: i64,
    file_b: i64,
    state: State<'_, AppState>,
) -> Result<athenaeum_core::duplicates::VerifyPairResult, String> {
    athenaeum_core::api::files::verify_duplicate_pair(&state.ctx, file_a, file_b)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn verify_files_byte_identical(
    path1: String,
    path2: String,
) -> Result<bool, String> {
    let p1 = std::path::PathBuf::from(&path1);
    let p2 = std::path::PathBuf::from(&path2);
    athenaeum_core::duplicates::verify_byte_identical(&p1, &p2).map_err(|e| e.to_string())
}
