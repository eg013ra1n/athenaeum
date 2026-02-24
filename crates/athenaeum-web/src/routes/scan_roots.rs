// Scan root route handlers — mirrors athenaeum-tauri/src/commands/scan_roots.rs

use athenaeum_core::db;
use athenaeum_core::models::ScanRoot;
use athenaeum_core::scanner::scan_directory_parallel;
use athenaeum_core::services::ScanHandle;
use axum::{extract::State, http::StatusCode, Json};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::events::SseProgressEmitter;
use crate::WebAppState;

// ── Response DTO ─────────────────────────────────────────────────────────────

/// Mirrors the ScanResultDto produced by the Tauri scan_roots commands.
#[derive(serde::Serialize)]
pub struct ScanResultDto {
    pub files_found: usize,
    pub files_processed: usize,
    pub files_skipped: usize,
    pub errors: Vec<String>,
    pub lights_count: usize,
    pub darks_count: usize,
    pub flats_count: usize,
    pub bias_count: usize,
    pub darkflats_count: usize,
    pub calibration_sets_created: usize,
    pub cancelled: bool,
    // Reconciliation fields from unique_camera instrume suffix reconciliation
    pub frames_renamed: usize,
    pub calibration_sets_deleted: usize,
    pub sessions_updated: usize,
}

// ── Request structs ──────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct AddScanRootArgs {
    pub path: String,
}

#[derive(serde::Deserialize)]
pub struct DeleteScanRootArgs {
    pub id: i64,
}

#[derive(serde::Deserialize)]
pub struct StartScanArgs {
    #[serde(rename = "rootId")]
    pub root_id: i64,
}

#[derive(serde::Deserialize)]
pub struct CancelScanArgs {
    #[serde(rename = "rootId")]
    pub root_id: i64,
}

#[derive(serde::Serialize)]
pub struct RescanResultDto {
    pub files_total: usize,
    pub files_updated: usize,
    pub files_skipped: usize,
    pub files_missing: usize,
    pub errors: Vec<String>,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// POST /api/add_scan_root
///
/// Validates the path exists, checks for overlaps with existing roots, then
/// inserts via `db::upsert_scan_root`. Returns the newly created `ScanRoot`.
pub async fn add_scan_root(
    State(state): State<WebAppState>,
    Json(args): Json<AddScanRootArgs>,
) -> Result<Json<ScanRoot>, (StatusCode, String)> {
    let path_buf = Path::new(&args.path).to_path_buf();

    if !path_buf.exists() {
        return Err((StatusCode::BAD_REQUEST, "Directory does not exist".to_string()));
    }
    if !path_buf.is_dir() {
        return Err((StatusCode::BAD_REQUEST, "Path is not a directory".to_string()));
    }

    let canonical = path_buf
        .canonicalize()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to resolve path: {}", e)))?;
    let path_str = canonical.to_string_lossy().to_string();

    // Security: validate path is within allowed_paths (web mode sandboxing)
    if !state.allowed_paths.is_empty() {
        let is_allowed = state.allowed_paths.iter().any(|allowed| {
            allowed.canonicalize().map(|a| canonical.starts_with(&a)).unwrap_or(false)
        });
        if !is_allowed {
            return Err((StatusCode::FORBIDDEN, "Path is outside allowed directories".to_string()));
        }
    }

    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    // Check for overlapping roots
    let existing = db::get_scan_roots(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    for root in &existing {
        let existing_canonical = Path::new(&root.path)
            .canonicalize()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to resolve existing root: {}", e)))?;
        let existing_str = existing_canonical.to_string_lossy().to_string();

        if path_str == existing_str {
            return Err((StatusCode::CONFLICT, "This directory is already being monitored".to_string()));
        }
        if canonical.starts_with(&existing_canonical) {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "Cannot add directory: it is a subdirectory of existing scan root '{}'",
                    root.path
                ),
            ));
        }
        if existing_canonical.starts_with(&canonical) {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "Cannot add directory: existing scan root '{}' is a subdirectory of it",
                    root.path
                ),
            ));
        }
    }

    let id = db::upsert_scan_root(&conn, &path_str)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(ScanRoot {
        id: Some(id),
        path: path_str,
        enabled: true,
        find_duplicates: true,
        unique_camera: false,
        last_scan: None,
        last_scan_errors: None,
    }))
}

/// POST /api/get_scan_roots
///
/// Returns all monitored directory paths from the database.
pub async fn get_scan_roots(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<Vec<ScanRoot>>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let roots = db::get_scan_roots(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(roots))
}

/// POST /api/delete_scan_root
///
/// Removes a scan root by ID. Does not delete files from disk.
pub async fn delete_scan_root(
    State(state): State<WebAppState>,
    Json(args): Json<DeleteScanRootArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    db::delete_scan_root(&conn, args.id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(()))
}

/// POST /api/start_scan_with_progress
///
/// Starts a parallel directory scan for the given root, emitting progress as
/// SSE events. The scan runs inside `spawn_blocking` since it is CPU-intensive.
/// SSE clients should listen on `GET /api/events` for `scan-progress` and
/// `scan-complete` events emitted by `SseProgressEmitter`.
pub async fn start_scan_with_progress(
    State(state): State<WebAppState>,
    Json(args): Json<StartScanArgs>,
) -> Result<Json<ScanResultDto>, (StatusCode, String)> {
    let root_id = args.root_id;

    // Reject duplicate concurrent scans for the same root
    {
        let scans = state.ctx.active_scans.lock().unwrap();
        if scans.contains_key(&root_id) {
            return Err((StatusCode::CONFLICT, "Scan already in progress for this root".to_string()));
        }
    }

    // Create cancellation flag and register the handle
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut scans = state.ctx.active_scans.lock().unwrap();
        scans.insert(
            root_id,
            ScanHandle {
                root_id,
                cancel_flag: cancel_flag.clone(),
            },
        );
    }

    // Gather everything needed before entering the blocking task
    let (scan_result, reconcile) = {
        let lock = state.ctx.db.lock().unwrap();
        let db = lock.as_ref().ok_or_else(|| {
            // Clean up active scan registration on early error
            let mut scans = state.ctx.active_scans.lock().unwrap();
            scans.remove(&root_id);
            (StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string())
        })?;
        let conn = db.conn();

        // Reconcile unique_camera instrume suffix state before scanning
        let reconcile = db::reconcile_unique_camera_instrume(&conn, root_id)
            .map_err(|e| {
                let mut scans = state.ctx.active_scans.lock().unwrap();
                scans.remove(&root_id);
                (StatusCode::INTERNAL_SERVER_ERROR, format!("Reconciliation failed: {}", e))
            })?;

        let roots = db::get_scan_roots(&conn).map_err(|e| {
            let mut scans = state.ctx.active_scans.lock().unwrap();
            scans.remove(&root_id);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

        let root = roots
            .into_iter()
            .find(|r| r.id == Some(root_id))
            .ok_or_else(|| {
                let mut scans = state.ctx.active_scans.lock().unwrap();
                scans.remove(&root_id);
                (StatusCode::NOT_FOUND, "Scan root not found".to_string())
            })?;

        let use_content_hash = state.ctx.settings
            .get_duplicates_use_content_hash(&conn)
            .unwrap_or(false);

        let emitter = SseProgressEmitter::new(state.event_tx.clone());

        // scan_directory_parallel holds the Connection reference for its full
        // duration, so it must run while the DB lock is held. The lock is
        // released when `lock` drops at the end of this block.
        let mut result = scan_directory_parallel(
            Path::new(&root.path),
            root_id,
            &conn,
            &emitter,
            use_content_hash,
            cancel_flag,
            root.unique_camera,
        );

        // If reconciliation changed frames, wipe and rebuild calibration sets
        if reconcile.frames_renamed > 0 {
            if let Err(e) = db::delete_calibration_sets_for_root(&conn, root_id) {
                eprintln!("Failed to delete cal sets for root {}: {}", root_id, e);
            }
            match recreate_calibration_sets_for_root(&conn, root_id) {
                Ok(count) => result.calibration_sets_created = count,
                Err(e) => eprintln!("Failed to recreate cal sets for root {}: {}", root_id, e),
            }
        }

        // Persist last-scan timestamp and any errors
        if let Err(e) = db::update_scan_root_timestamp(&conn, root_id) {
            eprintln!("Failed to update scan root timestamp for root {}: {}", root_id, e);
        }
        if let Err(e) = db::update_scan_root_errors(&conn, root_id, &result.errors) {
            eprintln!("Failed to persist scan errors for root {}: {}", root_id, e);
        }

        (result, reconcile)
    };

    // Deregister the active scan
    {
        let mut scans = state.ctx.active_scans.lock().unwrap();
        scans.remove(&root_id);
    }

    Ok(Json(ScanResultDto {
        files_found: scan_result.files_found,
        files_processed: scan_result.files_processed,
        files_skipped: scan_result.files_skipped,
        errors: scan_result.errors,
        lights_count: scan_result.lights_count,
        darks_count: scan_result.darks_count,
        flats_count: scan_result.flats_count,
        bias_count: scan_result.bias_count,
        darkflats_count: scan_result.darkflats_count,
        calibration_sets_created: scan_result.calibration_sets_created,
        cancelled: scan_result.cancelled,
        frames_renamed: reconcile.frames_renamed,
        calibration_sets_deleted: reconcile.calibration_sets_deleted,
        sessions_updated: reconcile.sessions_updated,
    }))
}

/// POST /api/cancel_scan
///
/// Sets the cancellation flag on an active scan. The scan will stop at its
/// next checkpoint and return a result with `cancelled: true`.
pub async fn cancel_scan(
    State(state): State<WebAppState>,
    Json(args): Json<CancelScanArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let scans = state.ctx.active_scans.lock().unwrap();
    if let Some(handle) = scans.get(&args.root_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        Ok(Json(()))
    } else {
        Err((StatusCode::NOT_FOUND, "No active scan for this root".to_string()))
    }
}

/// POST /api/check_all_scan_roots_availability
///
/// Returns a list of (root_id, exists) pairs indicating whether each scan root
/// directory is currently accessible on the filesystem.
pub async fn check_all_scan_roots_availability(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<Vec<(i64, bool)>>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let roots = db::get_scan_roots(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let availability: Vec<(i64, bool)> = roots
        .into_iter()
        .map(|root| {
            let exists = Path::new(&root.path).exists();
            (root.id.unwrap_or(0), exists)
        })
        .collect();

    Ok(Json(availability))
}

/// POST /api/get_missing_files_counts
///
/// Returns a map of scan_root_id -> count of files with 'missing' status.
pub async fn get_missing_files_counts(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<std::collections::HashMap<i64, i64>>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let mut stmt = conn
        .prepare(
            "SELECT scan_root_id, COUNT(*)
             FROM missing_files
             WHERE status = 'missing'
             GROUP BY scan_root_id",
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut counts = std::collections::HashMap::new();
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    for row in rows {
        let (root_id, count) = row.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        counts.insert(root_id, count);
    }

    Ok(Json(counts))
}

/// POST /api/start_scan
///
/// Non-progress scan variant. In web mode, delegates to start_scan_with_progress.
pub async fn start_scan(
    State(state): State<WebAppState>,
    Json(args): Json<StartScanArgs>,
) -> Result<Json<ScanResultDto>, (StatusCode, String)> {
    start_scan_with_progress(State(state), Json(args)).await
}

/// POST /api/rescan_all_for_content_hash
///
/// Iterate all files and compute xxHash for those missing a content_hash.
pub async fn rescan_all_for_content_hash(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<RescanResultDto>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    eprintln!("Starting content hash rescan for all files...");

    let all_files = athenaeum_core::db::get_files(&conn, None)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let total = all_files.len();

    let mut updated = 0;
    let mut skipped = 0;
    let mut missing = 0;
    let mut errors = Vec::new();

    for (file, _frame) in all_files {
        let path_buf = std::path::PathBuf::from(&file.path);

        if !path_buf.exists() {
            missing += 1;
            continue;
        }

        if file.content_hash.is_some() {
            skipped += 1;
            continue;
        }

        match athenaeum_core::duplicates::compute_xxhash(&path_buf) {
            Ok(hash) => {
                match conn.execute(
                    "UPDATE files SET content_hash = ?1 WHERE id = ?2",
                    rusqlite::params![hash, file.id],
                ) {
                    Ok(_) => {
                        updated += 1;
                        if updated % 100 == 0 {
                            eprintln!("Progress: {}/{} files processed", updated + skipped + missing, total);
                        }
                    }
                    Err(e) => {
                        errors.push(format!("{}: Failed to update database: {}", file.path, e));
                    }
                }
            }
            Err(e) => {
                errors.push(format!("{}: Failed to compute hash: {}", file.path, e));
            }
        }
    }

    eprintln!(
        "Content hash rescan complete! Total: {}, Updated: {}, Skipped: {}, Missing: {}, Errors: {}",
        total, updated, skipped, missing, errors.len()
    );

    if updated > 0 || skipped > 0 {
        let _ = state.ctx.settings
            .persist_setting(&conn, "duplicates.content_hash_rescanned", "true");
    }

    Ok(Json(RescanResultDto {
        files_total: total,
        files_updated: updated,
        files_skipped: skipped,
        files_missing: missing,
        errors,
    }))
}

/// POST /api/relink_scan_root — 501 in web mode (needs native dir picker)
pub async fn relink_scan_root(
    Json(_): Json<serde_json::Value>,
) -> (StatusCode, String) {
    (StatusCode::NOT_IMPLEMENTED, "relink_scan_root is not available in web mode".to_string())
}

/// POST /api/get_active_scans
///
/// Returns the list of root IDs that currently have an active scan running.
pub async fn get_active_scans(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<Vec<i64>>, (StatusCode, String)> {
    let scans = state.ctx.active_scans.lock().unwrap();
    Ok(Json(scans.keys().cloned().collect()))
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Query calibration frame IDs under a scan root and recreate calibration sets.
/// Mirrors `recreate_calibration_sets_for_root` in athenaeum-tauri.
fn recreate_calibration_sets_for_root(
    conn: &rusqlite::Connection,
    root_id: i64,
) -> Result<usize, String> {
    use athenaeum_core::calibration::scan_integration::{
        create_calibration_sets_from_scan_with_masters, MasterFrameIds,
    };

    let root_path: String = conn.query_row(
        "SELECT path FROM scan_roots WHERE id = ?1",
        rusqlite::params![root_id],
        |row| row.get(0),
    ).map_err(|e| e.to_string())?;

    let like_pattern = format!("{}%", root_path);

    let mut stmt = conn.prepare(
        "SELECT fr.id, fr.imagetyp FROM frames fr
         JOIN files f ON fr.file_id = f.id
         WHERE f.path LIKE ?1
           AND fr.imagetyp IN ('Flat','Dark','Bias','DarkFlat','MasterFlat','MasterDark','MasterBias','MasterDarkFlat')"
    ).map_err(|e| e.to_string())?;

    let mut flat_ids = Vec::new();
    let mut dark_ids = Vec::new();
    let mut bias_ids = Vec::new();
    let mut darkflat_ids = Vec::new();
    let mut master_ids = MasterFrameIds::default();

    let rows = stmt.query_map(rusqlite::params![like_pattern], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    }).map_err(|e| e.to_string())?;

    for row in rows {
        let (frame_id, imagetyp) = row.map_err(|e| e.to_string())?;
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

    eprintln!(
        "Recreating calibration sets for root {}: {} flats, {} darks, {} bias, {} darkflats, {} masters",
        root_id, flat_ids.len(), dark_ids.len(), bias_ids.len(),
        darkflat_ids.len(), master_ids.total_count()
    );

    let scan_result = create_calibration_sets_from_scan_with_masters(
        conn, flat_ids, dark_ids, bias_ids, darkflat_ids, master_ids,
    ).map_err(|e| e.to_string())?;

    Ok(scan_result.sets_created as usize)
}
