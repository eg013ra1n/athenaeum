// Scan root route handlers — mirrors athenaeum-tauri/src/commands/scan_roots.rs

use athenaeum_core::db;
use athenaeum_core::models::{RelinkResult, ScanRoot};
use axum::{extract::State, http::StatusCode, Json};
use std::path::Path;
use std::sync::atomic::Ordering;

use crate::events::SseProgressEmitter;
use crate::routes::files::path_inside_allowed;
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

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelinkScanRootArgs {
    pub root_id: i64,
    pub new_path: String,
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

    let db = state.ctx.db.get()
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
        monitor_enabled: false,
    }))
}

/// POST /api/get_scan_roots
///
/// Returns all monitored directory paths from the database.
pub async fn get_scan_roots(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<Vec<ScanRoot>>, (StatusCode, String)> {
    let db = state.ctx.db.get()
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
    let db = state.ctx.db.get()
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
    let emitter = SseProgressEmitter::new(state.event_tx.clone());

    let outcome = athenaeum_core::scanner::run_registered_scan(&state.ctx, &emitter, root_id)
        .map_err(|e| {
            let status = if e.contains("already in progress") {
                StatusCode::CONFLICT
            } else if e.contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, e)
        })?;
    let mut scan_result = outcome.result;
    let reconcile = outcome.reconcile;

    // Interactive-only follow-up: rebuild calibration sets if reconciliation renamed frames.
    if reconcile.frames_renamed > 0 {
        let db = state.ctx.db.get().ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database not initialized".to_string(),
        ))?;
        let conn = db.conn();
        if let Err(e) = db::delete_calibration_sets_for_root(&conn, root_id) {
            eprintln!("Failed to delete cal sets for root {}: {}", root_id, e);
        }
        match recreate_calibration_sets_for_root(&conn, root_id) {
            Ok(count) => scan_result.calibration_sets_created = count,
            Err(e) => eprintln!("Failed to recreate cal sets for root {}: {}", root_id, e),
        }
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
    let db = state.ctx.db.get()
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
    let db = state.ctx.db.get()
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
    let db = state.ctx.db.get()
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

/// POST /api/relink_scan_root
///
/// Path-input variant of the desktop `relink_scan_root` Tauri command
/// (`crates/athenaeum-tauri/src/commands/scan_roots.rs`) — web mode has no
/// native folder picker, so the caller supplies `new_path` directly and this
/// handler validates it server-side before mirroring the desktop command's
/// logic exactly: fetch the scan root's current path, delegate to
/// `athenaeum_core::relinking::relink_files` for the actual fingerprint-based
/// matching, then conditionally persist the new path.
pub async fn relink_scan_root(
    State(state): State<WebAppState>,
    Json(args): Json<RelinkScanRootArgs>,
) -> Result<Json<RelinkResult>, (StatusCode, String)> {
    let new_path_buf = Path::new(&args.new_path).to_path_buf();

    if !new_path_buf.exists() {
        return Err((StatusCode::BAD_REQUEST, "Directory does not exist".to_string()));
    }
    if !new_path_buf.is_dir() {
        return Err((StatusCode::BAD_REQUEST, "Path is not a directory".to_string()));
    }

    let canonical = new_path_buf.canonicalize().map_err(|e| {
        eprintln!("Failed to resolve relink target path '{}': {}", args.new_path, e);
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to resolve path: {}", e))
    })?;

    // Security: validate path is within allowed_paths (web mode sandboxing) —
    // same convention as add_scan_root above.
    if !state.allowed_paths.is_empty() && !path_inside_allowed(&canonical, &state.allowed_paths) {
        return Err((StatusCode::FORBIDDEN, "Path is outside allowed directories".to_string()));
    }

    let new_path = canonical.to_string_lossy().to_string();

    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    // Get old root path (mirrors the desktop command exactly).
    let old_path: String = conn
        .query_row(
            "SELECT path FROM scan_roots WHERE id = ?1",
            rusqlite::params![args.root_id],
            |row| row.get(0),
        )
        .map_err(|e| {
            eprintln!("Failed to get scan root {}: {}", args.root_id, e);
            if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                (StatusCode::NOT_FOUND, format!("Scan root {} not found", args.root_id))
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to get scan root: {}", e))
            }
        })?;

    println!("Relinking root {} from '{}' to '{}'", args.root_id, old_path, new_path);

    // Perform relinking (real logic in athenaeum-core, shared by both backends).
    let result = athenaeum_core::relinking::relink_files(&conn, &old_path, &new_path)
        .map_err(|e| {
            eprintln!("Relinking failed for root {}: {}", args.root_id, e);
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Relinking failed: {}", e))
        })?;

    // Update scan root path if all files were matched (mirrors desktop condition exactly).
    if result.files_orphaned == 0 || result.files_matched > 0 {
        conn.execute(
            "UPDATE scan_roots SET path = ?1 WHERE id = ?2",
            rusqlite::params![new_path, args.root_id],
        )
        .map_err(|e| {
            eprintln!("Failed to update scan root path for root {}: {}", args.root_id, e);
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to update scan root path: {}", e))
        })?;
        println!("Updated scan root path to '{}'", new_path);
    }

    Ok(Json(result))
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

#[derive(serde::Deserialize)]
pub struct SetMonitorEnabledArgs {
    pub id: i64,
    pub enabled: bool,
}

/// POST /api/set_scan_root_monitor_enabled
///
/// Toggle the background-monitoring flag for a scan root. The monitor service
/// picks up the new value on its next tick.
pub async fn set_scan_root_monitor_enabled(
    State(state): State<WebAppState>,
    Json(args): Json<SetMonitorEnabledArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "Database not initialized".to_string(),
    ))?;
    let conn = db.conn();
    db::set_scan_root_monitor_enabled(&conn, args.id, args.enabled)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Wake the monitor loop so the user gets an immediate scan instead of
    // waiting for the current sleep to finish. Only relevant when enabling.
    if args.enabled {
        state.monitor.kick();
    }

    Ok(Json(()))
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

#[cfg(test)]
mod relink_tests {
    use super::*;
    use athenaeum_core::cache::MemoryImageCache;
    use athenaeum_core::db::Database;
    use athenaeum_core::services::{operation_queue::OperationQueue, ServiceContext};
    use athenaeum_core::settings::SettingsManager;
    use crate::events::SseEvent;
    use rusqlite::{params, Connection};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, OnceLock, RwLock};
    use tempfile::TempDir;

    /// Real `mono.fits` fixture from the rustafits submodule (not a
    /// synthetic minimal FITS) so `relink_files`'s header-fingerprint
    /// matching runs against genuine header content — same fixture and
    /// rationale as `scanner::moved_file_guard_tests`.
    const MONO_FIXTURE: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../rustafits/tests/mono.fits");

    /// Builds a `WebAppState` backed by a real (file-based, temp) database —
    /// unlike `routes::tests::test_state` in `routes/mod.rs` (which
    /// deliberately leaves `ctx.db` unset to test only the auth layer),
    /// these tests exercise `relink_scan_root`'s actual
    /// `scan_roots`/`files`/`fits_header` reads and writes, so a real
    /// connection pool is required.
    fn test_state(db: Database, allowed_paths: Vec<PathBuf>) -> WebAppState {
        let db_cell = OnceLock::new();
        let _ = db_cell.set(db);
        let ctx = Arc::new(ServiceContext {
            db: db_cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            star_cache: Arc::new(RwLock::new(None)),
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
        });
        let (event_tx, _) = tokio::sync::broadcast::channel::<SseEvent>(16);
        WebAppState {
            ctx,
            event_tx,
            allowed_paths,
            export_dir: None,
            api_key: None,
            image_semaphore: Arc::new(RwLock::new(Arc::new(tokio::sync::Semaphore::new(1)))),
            max_blink_threads: 1,
            monitor: athenaeum_core::monitor::MonitorService::new(),
        }
    }

    /// Insert a fake `files` + `fits_header` row carrying the real
    /// fingerprint of `MONO_FIXTURE`, standing in for a "previously scanned"
    /// file at `path` (which is never actually written to disk — only the
    /// header content, hashed into the fingerprint, needs to match).
    fn seed_old_file_row(conn: &Connection, file_id: i64, path: &str, header_text: &str) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, 'old.fits', 100, '2025-01-01T00:00:00Z', 'FITS')",
            params![file_id, path],
        )
        .unwrap();
        athenaeum_core::db::insert_fits_header(conn, file_id, header_text).unwrap();
    }

    #[tokio::test]
    async fn relink_matches_file_by_fingerprint_and_updates_scan_root_path() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let conn = db.conn();

        let old_root = tmp.path().join("old-root"); // never created — it "moved away"
        let new_root = tmp.path().join("new-root");
        std::fs::create_dir_all(&new_root).unwrap();

        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [old_root.to_str().unwrap()],
        )
        .unwrap();

        let (_frame, header_text) = athenaeum_core::fits_parser::parse_fits_with_header(
            &PathBuf::from(MONO_FIXTURE), 0,
        ).unwrap();
        let old_path = old_root.join("L_001.fits");
        seed_old_file_row(&conn, 900, old_path.to_str().unwrap(), &header_text);

        // Same bytes at the new location -> same header -> same fingerprint.
        std::fs::copy(MONO_FIXTURE, new_root.join("L_001.fits")).unwrap();
        drop(conn);

        let state = test_state(db, Vec::new());
        let args = RelinkScanRootArgs { root_id: 1, new_path: new_root.to_string_lossy().to_string() };

        let result = relink_scan_root(State(state.clone()), Json(args)).await.unwrap().0;
        assert_eq!(result.files_matched, 1, "the copied fixture must match the seeded fingerprint");
        assert_eq!(result.files_orphaned, 0);

        let updated_path: String = state.ctx.db.get().unwrap().conn().query_row(
            "SELECT path FROM scan_roots WHERE id = 1",
            [],
            |row| row.get(0),
        ).unwrap();
        let expected = new_root.canonicalize().unwrap().to_string_lossy().to_string();
        assert_eq!(updated_path, expected, "scan_roots.path must be updated on a full match");
    }

    #[tokio::test]
    async fn relink_outside_allowed_paths_is_403_and_db_untouched() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let conn = db.conn();

        let old_root = tmp.path().join("old-root");
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [old_root.to_str().unwrap()],
        )
        .unwrap();
        drop(conn);

        let allowed_root = TempDir::new().unwrap();
        let outside_root = TempDir::new().unwrap(); // sibling temp dir, not under allowed_root

        let state = test_state(db, vec![allowed_root.path().to_path_buf()]);
        let args = RelinkScanRootArgs {
            root_id: 1,
            new_path: outside_root.path().to_string_lossy().to_string(),
        };

        let err = relink_scan_root(State(state.clone()), Json(args)).await.unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert_eq!(err.1, "Path is outside allowed directories");

        let path_after: String = state.ctx.db.get().unwrap().conn().query_row(
            "SELECT path FROM scan_roots WHERE id = 1",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(path_after, old_root.to_str().unwrap(), "DB must be untouched on 403");
    }

    #[tokio::test]
    async fn relink_nonexistent_new_path_is_400() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [tmp.path().join("old-root").to_str().unwrap()],
        )
        .unwrap();
        drop(conn);

        let state = test_state(db, Vec::new());
        let missing = tmp.path().join("does-not-exist");
        let args = RelinkScanRootArgs { root_id: 1, new_path: missing.to_string_lossy().to_string() };

        let err = relink_scan_root(State(state), Json(args)).await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn relink_unknown_root_id_is_404() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        // No scan_roots rows inserted — root_id 999 does not exist.

        let new_root = tmp.path().join("new-root");
        std::fs::create_dir_all(&new_root).unwrap();

        let state = test_state(db, Vec::new());
        let args = RelinkScanRootArgs { root_id: 999, new_path: new_root.to_string_lossy().to_string() };

        let err = relink_scan_root(State(state), Json(args)).await.unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }
}
