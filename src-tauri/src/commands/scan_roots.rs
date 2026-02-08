// Scan root commands - directory scanning and monitoring

use crate::db::{self};
use crate::models::*;
use crate::scanner::{scan_directory, scan_directory_parallel, emit_progress};
use rayon::prelude::*;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::State;

use super::{AppState, ScanHandle};
use super::utils::normalize_path;

#[tauri::command]
pub async fn add_scan_root(path: String, state: State<'_, AppState>) -> Result<ScanRoot, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // 1. Check if directory exists
    let path_buf = Path::new(&path);
    if !path_buf.exists() {
        return Err("Directory does not exist".to_string());
    }
    if !path_buf.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    // 2. Canonicalize the new path (resolve symlinks, .., etc.)
    //    normalize_path strips the \\?\ prefix that Windows canonicalize() adds
    let new_path = normalize_path(
        &path_buf
            .canonicalize()
            .map_err(|e| format!("Failed to resolve path: {}", e))?,
    );

    // 3. Get existing scan roots and check for overlaps
    let existing_roots = db::get_scan_roots(&conn).map_err(|e| e.to_string())?;

    for root in existing_roots.iter() {
        let existing_path = normalize_path(
            &Path::new(&root.path)
                .canonicalize()
                .map_err(|e| format!("Failed to resolve existing root path: {}", e))?,
        );

        // Check exact match
        if new_path == existing_path {
            return Err("This directory is already being monitored".to_string());
        }

        // Check if new path is a subdirectory of existing root
        if new_path.starts_with(&existing_path) {
            return Err(format!(
                "Cannot add directory: it is a subdirectory of existing scan root '{}'",
                root.path
            ));
        }

        // Check if new path is a parent of existing root
        if existing_path.starts_with(&new_path) {
            return Err(format!(
                "Cannot add directory: existing scan root '{}' is a subdirectory of it",
                root.path
            ));
        }
    }

    // 4. Store the canonicalized path
    let path_str = new_path.to_string_lossy().to_string();
    let id = db::upsert_scan_root(&conn, &path_str).map_err(|e| e.to_string())?;

    Ok(ScanRoot {
        id: Some(id),
        path: path_str,
        enabled: true,
        find_duplicates: true,
        unique_camera: false,
        last_scan: None,
    })
}

#[tauri::command]
pub async fn get_scan_roots(state: State<'_, AppState>) -> Result<Vec<ScanRoot>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_scan_roots(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_scan_root(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::delete_scan_root(&conn, id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_scan(root_id: i64, state: State<'_, AppState>) -> Result<ScanResultDto, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Reconcile unique_camera instrume suffix state before scanning
    let reconcile = db::reconcile_unique_camera_instrume(&conn, root_id)
        .map_err(|e| format!("Reconciliation failed: {}", e))?;

    // Get the scan root path
    let roots = db::get_scan_roots(&conn).map_err(|e| e.to_string())?;
    let root = roots
        .into_iter()
        .find(|r| r.id == Some(root_id))
        .ok_or("Scan root not found")?;

    // Check if content hash should be computed
    let use_content_hash = state.settings
        .get_duplicates_use_content_hash(&conn)
        .unwrap_or(false);

    // Perform the scan
    let mut result = scan_directory(Path::new(&root.path), &conn, None, use_content_hash, root.unique_camera, root_id);

    // If reconciliation changed frames, wipe and rebuild calibration sets
    if reconcile.frames_renamed > 0 {
        db::delete_calibration_sets_for_root(&conn, root_id)
            .map_err(|e| format!("Failed to delete cal sets: {}", e))?;
        let cal_sets_created = recreate_calibration_sets_for_root(&conn, root_id)
            .map_err(|e| format!("Failed to recreate cal sets: {}", e))?;
        result.calibration_sets_created = cal_sets_created;
    }

    // Update last_scan timestamp
    db::update_scan_root_timestamp(&conn, root_id).map_err(|e| e.to_string())?;

    Ok(ScanResultDto {
        files_found: result.files_found,
        files_processed: result.files_processed,
        files_skipped: result.files_skipped,
        errors: result.errors,
        lights_count: result.lights_count,
        darks_count: result.darks_count,
        flats_count: result.flats_count,
        bias_count: result.bias_count,
        darkflats_count: result.darkflats_count,
        calibration_sets_created: result.calibration_sets_created,
        cancelled: result.cancelled,
        frames_renamed: reconcile.frames_renamed,
        calibration_sets_deleted: reconcile.calibration_sets_deleted,
        sessions_updated: reconcile.sessions_updated,
    })
}

#[tauri::command]
pub async fn rescan_all_for_content_hash(state: State<'_, AppState>) -> Result<RescanResultDto, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    println!("Starting content hash rescan for all files...");

    // Get all files from database
    let all_files = db::get_files(&conn, None).map_err(|e| e.to_string())?;
    let total = all_files.len();

    let mut updated = 0;
    let mut skipped = 0;
    let mut missing = 0;
    let mut errors = Vec::new();

    for (file, _frame) in all_files {
        let path_buf = std::path::PathBuf::from(&file.path);

        // Skip if file doesn't exist on disk
        if !path_buf.exists() {
            missing += 1;
            continue;
        }

        // Skip if already has content hash
        if file.content_hash.is_some() {
            skipped += 1;
            continue;
        }

        // Compute content hash
        match crate::duplicates::compute_xxhash(&path_buf) {
            Ok(hash) => {
                // Update database
                match conn.execute(
                    "UPDATE files SET content_hash = ?1 WHERE id = ?2",
                    rusqlite::params![hash, file.id],
                ) {
                    Ok(_) => {
                        updated += 1;
                        if updated % 100 == 0 {
                            println!("Progress: {}/{} files processed", updated + skipped + missing, total);
                        }
                    }
                    Err(e) => {
                        let error_msg = format!("{}: Failed to update database: {}", file.path, e);
                        errors.push(error_msg);
                    }
                }
            }
            Err(e) => {
                let error_msg = format!("{}: Failed to compute hash: {}", file.path, e);
                errors.push(error_msg);
            }
        }
    }

    println!("Content hash rescan complete!");
    println!("Total: {}, Updated: {}, Skipped: {}, Missing: {}, Errors: {}",
        total, updated, skipped, missing, errors.len());

    // Mark content hash rescan as completed
    if updated > 0 || skipped > 0 {
        // Only set flag if we actually processed files successfully
        state.settings
            .persist_setting(&conn, "duplicates.content_hash_rescanned", "true")
            .map_err(|e| format!("Failed to set rescan flag: {}", e))?;
        println!("Content hash rescan flag set to true");
    }

    Ok(RescanResultDto {
        files_total: total,
        files_updated: updated,
        files_skipped: skipped,
        files_missing: missing,
        errors,
    })
}

/// Relink files from old scan root to new location
#[tauri::command]
pub async fn relink_scan_root(
    root_id: i64,
    new_path: String,
    state: State<'_, AppState>,
) -> Result<crate::models::RelinkResult, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get old root path
    let old_path: String = conn
        .query_row(
            "SELECT path FROM scan_roots WHERE id = ?1",
            rusqlite::params![root_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to get scan root: {}", e))?;

    println!("Relinking root {} from '{}' to '{}'", root_id, old_path, new_path);

    // Perform relinking
    let result = crate::relinking::relink_files(&conn, &old_path, &new_path)
        .map_err(|e| format!("Relinking failed: {}", e))?;

    // Update scan root path if all files were matched
    if result.files_orphaned == 0 || result.files_matched > 0 {
        conn.execute(
            "UPDATE scan_roots SET path = ?1 WHERE id = ?2",
            rusqlite::params![new_path, root_id],
        )
        .map_err(|e| format!("Failed to update scan root path: {}", e))?;
        println!("Updated scan root path to '{}'", new_path);
    }

    Ok(result)
}

/// Check if a scan root directory is available/accessible
#[tauri::command]
pub async fn check_scan_root_availability(
    root_id: i64,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get scan root path
    let path: String = conn
        .query_row(
            "SELECT path FROM scan_roots WHERE id = ?1",
            rusqlite::params![root_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to get scan root: {}", e))?;

    // Check if path exists
    Ok(std::path::Path::new(&path).exists())
}

/// Check availability of all scan roots
#[tauri::command]
pub async fn check_all_scan_roots_availability(
    state: State<'_, AppState>,
) -> Result<Vec<(i64, bool)>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let roots = db::get_scan_roots(&conn).map_err(|e| e.to_string())?;

    let availability: Vec<(i64, bool)> = roots
        .into_iter()
        .map(|root| {
            let exists = std::path::Path::new(&root.path).exists();
            (root.id.unwrap_or(0), exists)
        })
        .collect();

    Ok(availability)
}

/// Check for missing files within a scan root
#[tauri::command]
pub async fn check_missing_files_in_scan_root(
    root_id: i64,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::OrphanedFile>, String> {
    // Emit initial "verifying" phase progress
    emit_progress(&app_handle, root_id, 0, 0, None, "verifying");

    // Collect files from database with lock held briefly
    let files = {
        let state_lock = state.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();

        // Get scan root path
        let path: String = conn
            .query_row(
                "SELECT path FROM scan_roots WHERE id = ?1",
                rusqlite::params![root_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get scan root: {}", e))?;

        // Get all files under this scan root
        // Use LEFT JOIN instead of subqueries to avoid N+1 query problem
        let mut stmt = conn
            .prepare(
                "SELECT f.id, f.path, f.filename, f.size, f.modified_at,
                        CASE WHEN fr.id IS NOT NULL THEN 1 ELSE 0 END as has_frame,
                        fr.object,
                        fr.date_obs
                 FROM files f
                 LEFT JOIN frames fr ON fr.file_id = f.id
                 WHERE f.path LIKE ?1"
            )
            .map_err(|e| e.to_string())?;

        let path_prefix = format!("{}%", path);
        let result: Vec<crate::models::OrphanedFile> = stmt
            .query_map(rusqlite::params![path_prefix], |row| {
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
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        result
        // Lock is released here when state_lock goes out of scope
    };

    // Filter to only files that don't exist on disk - done in parallel
    // This is done OUTSIDE the lock since filesystem checks can be slow
    let total_files = files.len();

    // Use parallel iteration for filesystem checks (I/O bound operations)
    let missing_files: Vec<crate::models::OrphanedFile> = files
        .into_par_iter()
        .filter(|file| !std::path::Path::new(&file.path).exists())
        .collect();

    // Emit final progress
    emit_progress(&app_handle, root_id, total_files, total_files, None, "verifying");

    Ok(missing_files)
}

// DTOs for scan_roots commands
#[derive(serde::Serialize)]
pub struct ScanResultDto {
    pub files_found: usize,
    pub files_processed: usize,
    pub files_skipped: usize,
    pub errors: Vec<String>,
    // Frame type counts
    pub lights_count: usize,
    pub darks_count: usize,
    pub flats_count: usize,
    pub bias_count: usize,
    pub darkflats_count: usize,
    // Calibration sets created
    pub calibration_sets_created: usize,
    // Whether scan was cancelled by user
    pub cancelled: bool,
    // Unique camera reconciliation (only non-zero when instrume suffix state changed)
    pub frames_renamed: usize,
    pub calibration_sets_deleted: usize,
    pub sessions_updated: usize,
}

#[derive(serde::Serialize)]
pub struct RescanResultDto {
    pub files_total: usize,
    pub files_updated: usize,
    pub files_skipped: usize,
    pub files_missing: usize,
    pub errors: Vec<String>,
}

/// Toggle unique_camera flag (flag-only, cascade happens on re-scan)
#[tauri::command]
pub async fn set_scan_root_unique_camera_flag(
    id: i64,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::set_unique_camera_flag(&conn, id, enabled)
        .map_err(|e| e.to_string())?;

    println!("unique_camera flag set for root {}: enabled={}", id, enabled);

    Ok(())
}

/// Query calibration frame IDs under a scan root and recreate calibration sets
fn recreate_calibration_sets_for_root(
    conn: &rusqlite::Connection,
    root_id: i64,
) -> anyhow::Result<usize> {
    use crate::calibration::scan_integration::{
        create_calibration_sets_from_scan_with_masters, MasterFrameIds,
    };

    // Get root path
    let root_path: String = conn.query_row(
        "SELECT path FROM scan_roots WHERE id = ?1",
        rusqlite::params![root_id],
        |row| row.get(0),
    )?;

    let like_pattern = format!("{}%", root_path);

    // Query all calibration frame IDs under this root, grouped by imagetyp
    let mut stmt = conn.prepare(
        "SELECT fr.id, fr.imagetyp FROM frames fr
         JOIN files f ON fr.file_id = f.id
         WHERE f.path LIKE ?1
           AND fr.imagetyp IN ('Flat','Dark','Bias','DarkFlat','MasterFlat','MasterDark','MasterBias','MasterDarkFlat')"
    )?;

    let mut flat_ids = Vec::new();
    let mut dark_ids = Vec::new();
    let mut bias_ids = Vec::new();
    let mut darkflat_ids = Vec::new();
    let mut master_ids = MasterFrameIds::default();

    let rows = stmt.query_map(rusqlite::params![like_pattern], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in rows {
        let (frame_id, imagetyp) = row?;
        match imagetyp.as_str() {
            "Flat" => flat_ids.push(frame_id),
            "Dark" => dark_ids.push(frame_id),
            "Bias" => bias_ids.push(frame_id),
            "DarkFlat" => darkflat_ids.push(frame_id),
            "MasterFlat" => master_ids.master_flat_ids.push(frame_id),
            "MasterDark" => master_ids.master_dark_ids.push(frame_id),
            "MasterBias" => master_ids.master_bias_ids.push(frame_id),
            "MasterDarkFlat" => master_ids.master_darkflat_ids.push(frame_id),
            _ => {}
        }
    }

    let total_cal_frames = flat_ids.len() + dark_ids.len() + bias_ids.len()
        + darkflat_ids.len() + master_ids.total_count();

    if total_cal_frames == 0 {
        return Ok(0);
    }

    println!(
        "Recreating calibration sets for root {}: {} flats, {} darks, {} bias, {} darkflats, {} masters",
        root_id, flat_ids.len(), dark_ids.len(), bias_ids.len(),
        darkflat_ids.len(), master_ids.total_count()
    );

    let scan_result = create_calibration_sets_from_scan_with_masters(
        conn, flat_ids, dark_ids, bias_ids, darkflat_ids, master_ids,
    )?;

    Ok(scan_result.sets_created as usize)
}

/// Start a scan with progress events - runs synchronously but emits progress events
/// The frontend should call this and listen to scan-progress/scan-complete events
#[tauri::command]
pub async fn start_scan_with_progress(
    root_id: i64,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ScanResultDto, String> {
    println!("🔵 start_scan_with_progress called for root_id={}", root_id);

    // Check if already scanning this root
    {
        let scans = state.active_scans.lock().unwrap();
        if scans.contains_key(&root_id) {
            println!("🔴 Scan already in progress for root_id={}", root_id);
            return Err("Scan already in progress for this root".to_string());
        }
    }

    // Create cancel flag and register scan
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut scans = state.active_scans.lock().unwrap();
        scans.insert(root_id, ScanHandle {
            root_id,
            cancel_flag: cancel_flag.clone(),
        });
    }

    // Get scan root info and perform scan
    println!("🔵 Acquiring database lock for root_id={}", root_id);
    let (result, reconcile) = {
        let state_lock = state.db.lock().unwrap();
        println!("🔵 Database lock acquired for root_id={}", root_id);
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();

        // Reconcile unique_camera instrume suffix state before scanning
        let reconcile = db::reconcile_unique_camera_instrume(&conn, root_id)
            .map_err(|e| format!("Reconciliation failed: {}", e))?;

        let roots = db::get_scan_roots(&conn).map_err(|e| e.to_string())?;
        let root = roots
            .into_iter()
            .find(|r| r.id == Some(root_id))
            .ok_or("Scan root not found")?;

        let use_content_hash = state.settings
            .get_duplicates_use_content_hash(&conn)
            .unwrap_or(false);

        // Perform the parallel scan with progress events
        let mut result = scan_directory_parallel(
            Path::new(&root.path),
            root_id,
            &conn,
            &app_handle,
            use_content_hash,
            cancel_flag.clone(),
            root.unique_camera,
        );

        // If reconciliation changed frames, wipe and rebuild calibration sets
        if reconcile.frames_renamed > 0 {
            db::delete_calibration_sets_for_root(&conn, root_id)
                .map_err(|e| format!("Failed to delete cal sets: {}", e))?;
            let cal_sets_created = recreate_calibration_sets_for_root(&conn, root_id)
                .map_err(|e| format!("Failed to recreate cal sets: {}", e))?;
            result.calibration_sets_created = cal_sets_created;
        }

        // Update last_scan timestamp
        db::update_scan_root_timestamp(&conn, root_id).map_err(|e| e.to_string())?;

        println!("🔵 Scan complete for root_id={}, releasing lock", root_id);
        (result, reconcile)
    };

    println!("🔵 Database lock released for root_id={}", root_id);

    // Remove from active scans
    {
        let mut scans = state.active_scans.lock().unwrap();
        scans.remove(&root_id);
    }

    Ok(ScanResultDto {
        files_found: result.files_found,
        files_processed: result.files_processed,
        files_skipped: result.files_skipped,
        errors: result.errors,
        lights_count: result.lights_count,
        darks_count: result.darks_count,
        flats_count: result.flats_count,
        bias_count: result.bias_count,
        darkflats_count: result.darkflats_count,
        calibration_sets_created: result.calibration_sets_created,
        cancelled: result.cancelled,
        frames_renamed: reconcile.frames_renamed,
        calibration_sets_deleted: reconcile.calibration_sets_deleted,
        sessions_updated: reconcile.sessions_updated,
    })
}

/// Cancel an active scan
#[tauri::command]
pub async fn cancel_scan(
    root_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let scans = state.active_scans.lock().unwrap();
    if let Some(handle) = scans.get(&root_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        Ok(())
    } else {
        Err("No active scan for this root".to_string())
    }
}

/// Get list of active scan root IDs
#[tauri::command]
pub async fn get_active_scans(
    state: State<'_, AppState>,
) -> Result<Vec<i64>, String> {
    let scans = state.active_scans.lock().unwrap();
    Ok(scans.keys().cloned().collect())
}
