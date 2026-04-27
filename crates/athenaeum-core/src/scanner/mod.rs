// File scanner module
// Handles directory traversal and metadata extraction

use crate::db::{
    file_exists, insert_file, insert_frame, insert_fits_header,
    rebuild_duplicate_groups_cache, rebuild_folder_similarity_cache,
};
use crate::fits_parser::{parse_xisf, extract_xisf_header, parse_fits_with_header};
use crate::duplicates::compute_metadata_hash;
use crate::models::{File, Frame, FileFormat, ImageType};
use chrono::Utc;
use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use crate::events::{ProgressEmitter, emit_event};
use walkdir::WalkDir;

/// Convert a path to UTF-8 string for DB persistence.
/// Rejects non-UTF-8 paths instead of silently corrupting them via U+FFFD
/// replacement (which would break any subsequent path-based lookup).
fn path_to_utf8(path: &std::path::Path) -> anyhow::Result<String> {
    path.to_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!(
            "Path is not valid UTF-8 and cannot be persisted: {}",
            path.display()
        ))
}

/// Result from processing a single file (for collecting before batch insert)
#[derive(Clone)]
pub struct FileProcessResult {
    pub file: File,
    pub frame: Frame,
    pub header: Option<String>,
    pub imagetyp: Option<ImageType>,
    /// Non-fatal hash failure encountered during processing. The file was
    /// still parsed and is in the result; only its content_hash is None.
    pub hash_error: Option<String>,
}

/// Progress event sent to frontend via Tauri events
#[derive(Clone, serde::Serialize)]
pub struct ScanProgressEvent {
    pub current: usize,
    pub total: usize,
    pub current_file: Option<String>,
    pub percent: f64,
    pub root_id: i64,
    pub phase: String, // "discovery", "processing", "inserting", "calibrating"
}

/// Scan completion event
#[derive(Clone, serde::Serialize)]
pub struct ScanCompleteEvent {
    pub root_id: i64,
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
}

/// Scan a directory for FITS/XISF files
pub fn scan_directory(
    root_path: &Path,
    conn: &Connection,
    progress_callback: Option<Box<dyn Fn(ScanProgress) + Send + Sync>>,
    use_content_hash: bool,
    unique_camera: bool,
    root_id: i64,
) -> ScanResult {
    let mut result = ScanResult {
        files_found: 0,
        files_processed: 0,
        files_skipped: 0,
        errors: Vec::new(),
        lights_count: 0,
        darks_count: 0,
        flats_count: 0,
        bias_count: 0,
        darkflats_count: 0,
        calibration_sets_created: 0,
        cancelled: false,
    };

    // Find all FITS/XISF files. max_depth caps recursion in case follow_links
    // hits a pathological symlink loop (walkdir's loop detection isn't
    // bulletproof on every filesystem); 64 is well past any realistic archive.
    let files: Vec<PathBuf> = WalkDir::new(root_path)
        .follow_links(true)
        .max_depth(64)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            if let Some(ext) = e.path().extension() {
                let ext_str = ext.to_string_lossy().to_lowercase();
                matches!(ext_str.as_str(), "fits" | "fit" | "fts" | "xisf")
            } else {
                false
            }
        })
        .map(|e| e.path().to_path_buf())
        .collect();

    result.files_found = files.len();

    if let Some(ref cb) = progress_callback {
        cb(ScanProgress {
            current: 0,
            total: result.files_found,
            current_file: None,
        });
    }

    // Process files
    let errors = Arc::new(Mutex::new(Vec::new()));
    let processed = Arc::new(Mutex::new(0usize));
    let skipped = Arc::new(Mutex::new(0usize));

    // Track calibration frame IDs for automatic set creation
    let mut flat_frame_ids: Vec<i64> = Vec::new();
    let mut dark_frame_ids: Vec<i64> = Vec::new();
    let mut bias_frame_ids: Vec<i64> = Vec::new();
    let mut darkflat_frame_ids: Vec<i64> = Vec::new();
    let mut lights_count: usize = 0;

    // Track master calibration frame IDs
    let mut master_dark_ids: Vec<i64> = Vec::new();
    let mut master_flat_ids: Vec<i64> = Vec::new();
    let mut master_bias_ids: Vec<i64> = Vec::new();
    let mut master_darkflat_ids: Vec<i64> = Vec::new();

    for (idx, file_path) in files.iter().enumerate() {
        if let Some(ref cb) = progress_callback {
            cb(ScanProgress {
                current: idx + 1,
                total: result.files_found,
                current_file: Some(file_path.to_string_lossy().to_string()),
            });
        }

        // Skip if already in database AND on-disk metadata still matches.
        // If size or modified_at changed, treat as "modified" — purge the
        // stale row (CASCADE clears frames/headers) so process_file can
        // re-insert clean state.
        let path_str = file_path.to_string_lossy().to_string();
        let existing = conn
            .query_row(
                "SELECT size, modified_at FROM files WHERE path = ?1",
                rusqlite::params![path_str],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .ok();
        if let Some((db_size, db_modified)) = existing {
            let on_disk = std::fs::metadata(file_path).ok().map(|m| {
                let size = m.len() as i64;
                let modified = m
                    .modified()
                    .ok()
                    .map(|t| chrono::DateTime::<Utc>::from(t).to_rfc3339());
                (size, modified)
            });
            let unchanged = matches!(
                on_disk.as_ref(),
                Some((s, Some(m))) if *s == db_size && m.as_str() == db_modified
            );
            if unchanged {
                *skipped.lock().unwrap() += 1;
                continue;
            }
            // Modified in place — purge the stale row before re-processing.
            if let Err(e) = conn.execute("DELETE FROM files WHERE path = ?1", rusqlite::params![path_str]) {
                errors.lock().unwrap().push(format!(
                    "{}: failed to purge stale row before re-process: {}",
                    file_path.display(),
                    e
                ));
                continue;
            }
        }

        let mut hash_errors_local: Vec<String> = Vec::new();
        match process_file(
            file_path,
            conn,
            use_content_hash,
            unique_camera,
            root_id,
            &mut hash_errors_local,
        ) {
            Ok(frame_info) => {
                *processed.lock().unwrap() += 1;

                // Collect frame IDs by type
                if let Some((frame_id, imagetyp)) = frame_info {
                    match imagetyp {
                        ImageType::Light => lights_count += 1,
                        ImageType::Flat => flat_frame_ids.push(frame_id),
                        ImageType::Dark => dark_frame_ids.push(frame_id),
                        ImageType::Bias => bias_frame_ids.push(frame_id),
                        ImageType::DarkFlat => darkflat_frame_ids.push(frame_id),
                        // Master calibration frames
                        ImageType::MasterDark => master_dark_ids.push(frame_id),
                        ImageType::MasterFlat => master_flat_ids.push(frame_id),
                        ImageType::MasterBias => master_bias_ids.push(frame_id),
                        ImageType::MasterDarkFlat => master_darkflat_ids.push(frame_id),
                        _ => {} // Unknown types (MasterLight) - no tracking
                    }
                }
            }
            Err(e) => {
                errors
                    .lock()
                    .unwrap()
                    .push(format!("{}: {}", file_path.display(), e));
            }
        }
        if !hash_errors_local.is_empty() {
            errors.lock().unwrap().extend(hash_errors_local);
        }
    }

    result.files_processed = *processed.lock().unwrap();
    result.files_skipped = *skipped.lock().unwrap();
    result.errors = match Arc::try_unwrap(errors) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc) => arc.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    };

    // Populate frame type counts
    result.lights_count = lights_count;
    result.flats_count = flat_frame_ids.len();
    result.darks_count = dark_frame_ids.len();
    result.bias_count = bias_frame_ids.len();
    result.darkflats_count = darkflat_frame_ids.len();

    // Create calibration sets from newly scanned calibration frames
    let has_calibration_frames = !flat_frame_ids.is_empty()
        || !dark_frame_ids.is_empty()
        || !bias_frame_ids.is_empty()
        || !darkflat_frame_ids.is_empty();

    let has_master_frames = !master_dark_ids.is_empty()
        || !master_flat_ids.is_empty()
        || !master_bias_ids.is_empty()
        || !master_darkflat_ids.is_empty();

    if has_calibration_frames || has_master_frames {
        use crate::calibration::scan_integration::{create_calibration_sets_from_scan_with_masters, MasterFrameIds};

        let master_frame_ids = MasterFrameIds {
            master_dark_ids,
            master_flat_ids,
            master_bias_ids,
            master_darkflat_ids,
        };

        match create_calibration_sets_from_scan_with_masters(
            conn,
            flat_frame_ids,
            dark_frame_ids,
            bias_frame_ids,
            darkflat_frame_ids,
            master_frame_ids,
        ) {
            Ok(cal_result) => {
                result.calibration_sets_created = cal_result.sets_created as usize;
                let master_total = cal_result.master_dark_sets_created
                    + cal_result.master_flat_sets_created
                    + cal_result.master_bias_sets_created
                    + cal_result.master_darkflat_sets_created;
                if master_total > 0 {
                    println!("Auto-created {} calibration sets from scan ({} master)", cal_result.sets_created, master_total);
                } else {
                    println!("Auto-created {} calibration sets from scan", cal_result.sets_created);
                }
            }
            Err(e) => {
                // Surface errors to user instead of just logging
                result.errors.push(format!("Failed to auto-create calibration sets: {}", e));
                println!("Warning: Failed to auto-create calibration sets: {}", e);
            }
        }
    }

    result
}

/// Process a single file: hash, parse metadata, insert into database
/// Returns Some((frame_id, imagetyp)) for successfully processed frames with known imagetyp
/// Any non-fatal hash failures are pushed to `hash_errors_out` (prefixed `hash_error:`).
fn process_file(
    path: &PathBuf,
    conn: &Connection,
    use_content_hash: bool,
    unique_camera: bool,
    root_id: i64,
    hash_errors_out: &mut Vec<String>,
) -> anyhow::Result<Option<(i64, ImageType)>> {
    // Get file metadata
    let metadata = std::fs::metadata(path)?;
    let size = metadata.len() as i64;
    let modified = metadata.modified()?;
    let modified_dt = chrono::DateTime::<Utc>::from(modified);

    // Determine format
    let format = if let Some(ext) = path.extension() {
        let ext_str = ext.to_string_lossy().to_lowercase();
        if ext_str == "xisf" {
            FileFormat::XISF
        } else {
            FileFormat::FITS
        }
    } else {
        FileFormat::FITS
    };

    // Extract filename
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    let current_path = path_to_utf8(path)?;

    // Check if file already exists at this exact path
    if file_exists(conn, &current_path)? {
        // File already indexed at this path - skip
        return Ok(None);
    }

    // Parse metadata and header in a single read for FITS files
    // This also serves for the moved-file fingerprint check
    let (early_frame, header_text) = match format {
        FileFormat::FITS => {
            let (f, h) = parse_fits_with_header(path, 0)?;
            (Some(f), Some(h))
        }
        FileFormat::XISF => {
            // XISF reads are already bounded to 1MB, no full-file read issue
            (None, extract_xisf_header(path).ok())
        }
    };

    // Check for moved files using the header fingerprint
    if let Some(ref header) = header_text {
        let fingerprint = crate::fingerprint::compute_header_fingerprint(header);

        let existing_file: Option<(i64, String)> = conn.query_row(
            "SELECT f.id, f.path FROM files f
             INNER JOIN fits_header fh ON f.id = fh.file_id
             WHERE fh.header_fingerprint = ?1 AND f.path != ?2",
            rusqlite::params![fingerprint, current_path],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;

        if let Some((file_id, old_path)) = existing_file {
            if std::path::Path::new(&old_path).exists() {
                println!("Detected duplicate file: '{}' and '{}' have identical headers", old_path, current_path);
            } else {
                println!("Detected moved file: '{}' -> '{}' (file_id={})", old_path, current_path, file_id);

                conn.execute(
                    "UPDATE files SET path = ?1, modified_at = ?2 WHERE id = ?3",
                    rusqlite::params![current_path, modified_dt.to_rfc3339(), file_id],
                )?;

                let raw_instrume: Option<String> = conn.query_row(
                    "SELECT instrume FROM frames WHERE file_id = ?1",
                    rusqlite::params![file_id],
                    |row| row.get(0),
                ).optional()?.flatten();
                let base_instrume = raw_instrume.as_ref().map(|i| {
                    if let Some(pos) = i.rfind(" N") {
                        let suffix = &i[pos + 2..];
                        if suffix.chars().all(|c| c.is_ascii_digit()) && !suffix.is_empty() {
                            return i[..pos].to_string();
                        }
                    }
                    i.clone()
                });
                let new_instrume = if unique_camera {
                    base_instrume.map(|i| format!("{} N{}", i, root_id))
                } else {
                    base_instrume
                };
                let _ = conn.execute(
                    "UPDATE frames SET instrume = ?1 WHERE file_id = ?2",
                    rusqlite::params![new_instrume, file_id],
                );

                return Ok(None);
            }
        }
    }

    // If we get here, it's a truly new file - insert it
    let metadata_hash = compute_metadata_hash(size, &modified_dt, &filename);

    let content_hash = if use_content_hash {
        match crate::duplicates::compute_xxhash(path) {
            Ok(hash) => {
                println!("Computed content hash for '{}': {}", current_path, hash);
                Some(hash)
            }
            Err(e) => {
                // Surface the failure so the user knows duplicate detection
                // skipped this file. Previously silently dropped to None.
                let msg = format!("hash_error: {}: failed to compute content hash: {}", current_path, e);
                crate::logging::log("WARN", &msg);
                hash_errors_out.push(msg);
                None
            }
        }
    } else {
        None
    };

    let file = File {
        id: None,
        path: current_path,
        filename,
        size,
        modified_at: modified_dt,
        format: format.clone(),
        created_at: Utc::now(),
        metadata_hash: Some(metadata_hash),
        content_hash,
    };

    let file_id = insert_file(conn, &file)?;

    // Use already-parsed frame for FITS, or parse fresh for XISF
    let mut frame = match early_frame {
        Some(mut f) => {
            f.file_id = file_id;
            f
        }
        None => parse_xisf(path, file_id)?,
    };

    // Apply unique camera suffix to INSTRUME if enabled
    if unique_camera {
        if let Some(ref instrume) = frame.instrume {
            frame.instrume = Some(format!("{} N{}", instrume, root_id));
        }
    }

    let frame_id = insert_frame(conn, &frame)?;
    let imagetyp = frame.imagetyp.clone();

    // Store header for future reference (already extracted above)
    if let Some(header) = header_text {
        println!("Storing header for file_id={}, header length={} bytes", file_id, header.len());
        if let Err(e) = insert_fits_header(conn, file_id, &header) {
            println!("Warning: Failed to store header: {}", e);
        }
    } else if format == FileFormat::XISF {
        // XISF header extraction failed earlier, try again
        match extract_xisf_header(path) {
            Ok(header) => {
                println!("Storing XISF header for file_id={}, header length={} bytes", file_id, header.len());
                if let Err(e) = insert_fits_header(conn, file_id, &header) {
                    println!("Warning: Failed to store XISF header: {}", e);
                }
            }
            Err(e) => {
                println!("Warning: Failed to extract XISF header: {}", e);
            }
        }
    }

    // Return frame info if imagetyp is known
    Ok(imagetyp.map(|it| (frame_id, it)))
}

pub struct ScanResult {
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
}

#[allow(dead_code)]
pub struct ScanProgress {
    pub current: usize,
    pub total: usize,
    pub current_file: Option<String>,
}

/// Emit progress event to frontend (throttled)
pub fn emit_progress<E: ProgressEmitter>(
    emitter: &E,
    root_id: i64,
    current: usize,
    total: usize,
    current_file: Option<String>,
    phase: &str,
) {
    let percent = if total > 0 {
        (current as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    let event = ScanProgressEvent {
        current,
        total,
        current_file,
        percent,
        root_id,
        phase: phase.to_string(),
    };

    emit_event(emitter, "scan-progress", &event);
}

/// Emit scan complete event to frontend
fn emit_scan_complete<E: ProgressEmitter>(emitter: &E, root_id: i64, result: &ScanResult) {
    let event = ScanCompleteEvent {
        root_id,
        files_found: result.files_found,
        files_processed: result.files_processed,
        files_skipped: result.files_skipped,
        errors: result.errors.clone(),
        lights_count: result.lights_count,
        darks_count: result.darks_count,
        flats_count: result.flats_count,
        bias_count: result.bias_count,
        darkflats_count: result.darkflats_count,
        calibration_sets_created: result.calibration_sets_created,
        cancelled: result.cancelled,
    };

    emit_event(emitter, "scan-complete", &event);
}

/// Process a single file without database access (safe for parallel execution)
/// Returns file data ready for batch insertion
fn process_file_parallel(
    path: &PathBuf,
    use_content_hash: bool,
) -> Result<FileProcessResult, String> {
    crate::logging::log("DEBUG", &format!("Processing: {}", path.display()));

    // Get file metadata
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let size = metadata.len() as i64;
    let modified = metadata.modified().map_err(|e| e.to_string())?;
    let modified_dt = chrono::DateTime::<Utc>::from(modified);

    // Determine format
    let format = if let Some(ext) = path.extension() {
        let ext_str = ext.to_string_lossy().to_lowercase();
        if ext_str == "xisf" {
            FileFormat::XISF
        } else {
            FileFormat::FITS
        }
    } else {
        FileFormat::FITS
    };

    // Extract filename
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    let current_path = path_to_utf8(path).map_err(|e| e.to_string())?;

    // Compute metadata hash
    let metadata_hash = compute_metadata_hash(size, &modified_dt, &filename);

    // Compute content hash if enabled. Surface failures to the caller as
    // hash_error so duplicate detection coverage gaps are visible to the user.
    let (content_hash, hash_error) = if use_content_hash {
        match crate::duplicates::compute_xxhash(path) {
            Ok(hash) => (Some(hash), None),
            Err(e) => {
                let msg = format!(
                    "hash_error: {}: failed to compute content hash: {}",
                    path.display(),
                    e
                );
                (None, Some(msg))
            }
        }
    } else {
        (None, None)
    };

    // Create file record (id will be assigned during insert)
    let file = File {
        id: None,
        path: current_path,
        filename,
        size,
        modified_at: modified_dt,
        format: format.clone(),
        created_at: Utc::now(),
        metadata_hash: Some(metadata_hash),
        content_hash,
    };

    // Parse metadata and extract header in a single read for FITS files
    let (frame, header) = match format {
        FileFormat::FITS => {
            let (f, h) = parse_fits_with_header(path, 0).map_err(|e| e.to_string())?;
            (f, Some(h))
        }
        FileFormat::XISF => {
            let f = parse_xisf(path, 0).map_err(|e| e.to_string())?;
            let h = extract_xisf_header(path).ok();
            (f, h)
        }
    };

    let imagetyp = frame.imagetyp.clone();

    Ok(FileProcessResult {
        file,
        frame,
        header,
        imagetyp,
        hash_error,
    })
}

/// Parallel scan a directory for FITS/XISF files with progress reporting
///
/// This function uses a two-phase approach:
/// - Phase 1: Parallel file discovery and metadata extraction (CPU-bound)
/// - Phase 2: Sequential database inserts in a single transaction (I/O-bound)
pub fn scan_directory_parallel<E: ProgressEmitter>(
    root_path: &Path,
    root_id: i64,
    conn: &Connection,
    emitter: &E,
    use_content_hash: bool,
    cancel_flag: Arc<AtomicBool>,
    unique_camera: bool,
) -> ScanResult {
    let mut result = ScanResult {
        files_found: 0,
        files_processed: 0,
        files_skipped: 0,
        errors: Vec::new(),
        lights_count: 0,
        darks_count: 0,
        flats_count: 0,
        bias_count: 0,
        darkflats_count: 0,
        calibration_sets_created: 0,
        cancelled: false,
    };

    // Phase 1a: Discovery - collect all file paths with progress updates
    crate::logging::log("INFO", &format!("Phase 1a: Starting file discovery in '{}'", root_path.display()));
    emit_progress(emitter, root_id, 0, 0, None, "discovery");

    let mut files: Vec<PathBuf> = Vec::new();
    let mut discovery_count = 0usize;

    for entry in WalkDir::new(root_path)
        .follow_links(true)
        .max_depth(64)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        if let Some(ext) = entry.path().extension() {
            let ext_str = ext.to_string_lossy().to_lowercase();
            if matches!(ext_str.as_str(), "fits" | "fit" | "fts" | "xisf") {
                files.push(entry.path().to_path_buf());
                discovery_count += 1;

                // Emit progress every 100 files discovered
                if discovery_count % 100 == 0 {
                    emit_progress(
                        emitter,
                        root_id,
                        discovery_count,
                        0, // Total unknown during discovery
                        entry.path().file_name().map(|n| n.to_string_lossy().to_string()),
                        "discovery",
                    );
                }
            }
        }

        // Check for cancellation during discovery
        if discovery_count % 500 == 0 && cancel_flag.load(Ordering::SeqCst) {
            result.cancelled = true;
            result.files_found = files.len();
            emit_scan_complete(emitter, root_id, &result);
            return result;
        }
    }

    result.files_found = files.len();
    crate::logging::log("INFO", &format!("Phase 1a complete: {} files found", files.len()));

    // Check for cancellation before proceeding
    if cancel_flag.load(Ordering::SeqCst) {
        result.cancelled = true;
        emit_scan_complete(emitter, root_id, &result);
        return result;
    }

    if files.is_empty() {
        // Emit progress showing discovery complete with 0 files
        emit_progress(emitter, root_id, 0, 0, None, "processing");
        std::thread::sleep(std::time::Duration::from_millis(100));
        emit_scan_complete(emitter, root_id, &result);
        return result;
    }

    // Build a map of existing file paths to their stored (size, modified_at).
    // We use this to classify each on-disk file as new / unchanged / modified
    // so that in-place modifications get re-parsed instead of silently
    // skipped (the previous path-only filter). modified_at is stored as
    // RFC3339 in the DB and compared as a string for an exact-match check.
    crate::logging::log("INFO", "Building existing files map from DB...");
    let existing_files: std::collections::HashMap<String, (i64, String)> = {
        let mut map = std::collections::HashMap::new();
        match conn.prepare("SELECT path, size, modified_at FROM files") {
            Ok(mut stmt) => {
                match stmt.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
                }) {
                    Ok(rows) => {
                        for entry in rows.flatten() {
                            map.insert(entry.0, (entry.1, entry.2));
                        }
                    }
                    Err(e) => {
                        crate::logging::log("ERROR", &format!("Failed to query existing files: {}", e));
                    }
                }
            }
            Err(e) => {
                crate::logging::log("ERROR", &format!("Failed to prepare existing files query: {}", e));
            }
        }
        map
    };

    // Classify each discovered file. Modified files are re-processed by
    // deleting the old row first (CASCADE removes frames/headers/etc.).
    let mut modified_paths_to_purge: Vec<String> = Vec::new();
    let new_files: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| {
            let path_str = p.to_string_lossy().to_string();
            match existing_files.get(&path_str) {
                None => true, // New file
                Some((db_size, db_modified)) => {
                    // Compare against current on-disk metadata. If we can't
                    // stat the file, treat it as unchanged (the parallel
                    // process_file_parallel will surface the I/O error).
                    match std::fs::metadata(p) {
                        Ok(meta) => {
                            let on_disk_size = meta.len() as i64;
                            let on_disk_modified = meta
                                .modified()
                                .ok()
                                .map(|t| chrono::DateTime::<Utc>::from(t).to_rfc3339());
                            let unchanged = on_disk_size == *db_size
                                && on_disk_modified.as_deref() == Some(db_modified.as_str());
                            if unchanged {
                                false
                            } else {
                                modified_paths_to_purge.push(path_str);
                                true
                            }
                        }
                        Err(_) => false,
                    }
                }
            }
        })
        .collect();

    // Purge stale rows for modified files so insert can proceed cleanly.
    // Wrapped in a small transaction to keep the cascade atomic.
    if !modified_paths_to_purge.is_empty() {
        crate::logging::log(
            "INFO",
            &format!(
                "Detected {} modified file(s); purging stale catalog rows for re-processing",
                modified_paths_to_purge.len()
            ),
        );
        if let Err(e) = conn.execute("BEGIN TRANSACTION", []) {
            crate::logging::log("ERROR", &format!("M1 purge: BEGIN failed: {}", e));
        } else {
            let mut purge_failed = false;
            for path in &modified_paths_to_purge {
                if let Err(e) = conn.execute("DELETE FROM files WHERE path = ?1", rusqlite::params![path]) {
                    crate::logging::log("ERROR", &format!("M1 purge: DELETE failed for {}: {}", path, e));
                    result.errors.push(format!("Failed to purge stale row for modified file {}: {}", path, e));
                    purge_failed = true;
                    break;
                }
            }
            let final_stmt = if purge_failed { "ROLLBACK" } else { "COMMIT" };
            if let Err(e) = conn.execute(final_stmt, []) {
                crate::logging::log("ERROR", &format!("M1 purge: {} failed: {}", final_stmt, e));
            }
        }
    }

    result.files_skipped = result.files_found - new_files.len();
    crate::logging::log(
        "INFO",
        &format!(
            "Files to process: {} ({} unchanged, {} modified)",
            new_files.len(),
            result.files_skipped,
            modified_paths_to_purge.len()
        ),
    );

    // Check for cancellation before processing
    if cancel_flag.load(Ordering::SeqCst) {
        result.cancelled = true;
        emit_scan_complete(emitter, root_id, &result);
        return result;
    }

    if new_files.is_empty() {
        // Emit progress showing all files were checked (even if all skipped)
        // This ensures frontend always sees at least one progress event
        emit_progress(
            emitter,
            root_id,
            result.files_found,
            result.files_found,
            None,
            "processing",
        );
        // Small delay to ensure frontend has time to render progress modal
        std::thread::sleep(std::time::Duration::from_millis(100));
        emit_scan_complete(emitter, root_id, &result);
        return result;
    }

    // Phase 1b: Parallel processing - extract metadata from all files
    crate::logging::log("INFO", &format!("Phase 1b: Starting parallel FITS parsing of {} files", new_files.len()));
    let progress_counter = Arc::new(AtomicUsize::new(0));
    let total_new = new_files.len();
    let errors = Arc::new(Mutex::new(Vec::new()));
    let cancel_flag_clone = cancel_flag.clone();

    let mut processed_results: Vec<FileProcessResult> = new_files
        .par_iter()
        .filter_map(|path| {
            // Check for cancellation
            if cancel_flag_clone.load(Ordering::SeqCst) {
                return None;
            }

            let current = progress_counter.fetch_add(1, Ordering::SeqCst) + 1;

            // Emit progress every 10 files or on last file
            if current % 10 == 0 || current == total_new {
                emit_progress(
                    emitter,
                    root_id,
                    current,
                    total_new,
                    path.file_name().map(|n| n.to_string_lossy().to_string()),
                    "processing",
                );
            }

            // Catch panics from FITS/XISF parsing (rare unwrap()s on malformed
            // headers). Without this, rayon swallows the panic and the file
            // silently disappears from the result with no error trail.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                process_file_parallel(path, use_content_hash)
            }));

            match outcome {
                Ok(Ok(r)) => Some(r),
                Ok(Err(e)) => {
                    errors
                        .lock()
                        .unwrap()
                        .push(format!("{}: {}", path.display(), e));
                    None
                }
                Err(panic_payload) => {
                    let msg = panic_payload
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| {
                            panic_payload
                                .downcast_ref::<&'static str>()
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_else(|| "<non-string panic payload>".to_string());
                    errors
                        .lock()
                        .unwrap()
                        .push(format!("{}: panic during processing: {}", path.display(), msg));
                    None
                }
            }
        })
        .collect();

    crate::logging::log("INFO", &format!("Phase 1b complete: {} results collected", processed_results.len()));

    // Check if cancelled during processing phase
    if cancel_flag.load(Ordering::SeqCst) {
        result.cancelled = true;
        result.errors = match Arc::try_unwrap(errors) {
            Ok(mutex) => mutex.into_inner().unwrap_or_default(),
            Err(arc) => arc.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        };
        emit_scan_complete(emitter, root_id, &result);
        return result;
    }

    // Phase 2: Sequential database inserts in a transaction
    emit_progress(emitter, root_id, 0, processed_results.len(), None, "inserting");

    // Collect non-fatal hash errors from Phase 1 so users see which files
    // had no content_hash computed (duplicate detection coverage gaps).
    for r in processed_results.iter() {
        if let Some(ref msg) = r.hash_error {
            errors.lock().unwrap().push(msg.clone());
        }
    }

    // Track calibration frame IDs
    let mut flat_frame_ids: Vec<i64> = Vec::new();
    let mut dark_frame_ids: Vec<i64> = Vec::new();
    let mut bias_frame_ids: Vec<i64> = Vec::new();
    let mut darkflat_frame_ids: Vec<i64> = Vec::new();
    let mut master_dark_ids: Vec<i64> = Vec::new();
    let mut master_flat_ids: Vec<i64> = Vec::new();
    let mut master_bias_ids: Vec<i64> = Vec::new();
    let mut master_darkflat_ids: Vec<i64> = Vec::new();
    let mut lights_count: usize = 0;

    // Begin transaction for batch insert
    crate::logging::log("INFO", &format!("Phase 2: Starting DB inserts for {} results", processed_results.len()));
    if let Err(e) = conn.execute("BEGIN TRANSACTION", []) {
        crate::logging::log("ERROR", &format!("Phase 2: BEGIN TRANSACTION failed: {}", e));
        result.errors.push(format!("Failed to start DB transaction: {}", e));
        emit_scan_complete(emitter, root_id, &result);
        return result;
    }

    // Use iter_mut to avoid cloning Frame structs during insert
    let total_results = processed_results.len();
    let mut cancelled_mid_insert = false;
    for (idx, file_result) in processed_results.iter_mut().enumerate() {
        // Check for cancellation every 10 files
        if idx % 10 == 0 && cancel_flag.load(Ordering::SeqCst) {
            result.cancelled = true;
            cancelled_mid_insert = true;
            break;
        }

        // Emit progress every 50 files during insert phase
        if idx % 50 == 0 || idx == total_results - 1 {
            emit_progress(
                emitter,
                root_id,
                idx + 1,
                total_results,
                Some(file_result.file.filename.clone()),
                "inserting",
            );
        }

        // Check for moved files (same fingerprint at different path)
        if let Some(ref header) = file_result.header {
            let fingerprint = crate::fingerprint::compute_header_fingerprint(header);

            let existing_file: Option<(i64, String)> = conn.query_row(
                "SELECT f.id, f.path FROM files f
                 INNER JOIN fits_header fh ON f.id = fh.file_id
                 WHERE fh.header_fingerprint = ?1 AND f.path != ?2",
                rusqlite::params![fingerprint, file_result.file.path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional().unwrap_or(None);

            if let Some((file_id, old_path)) = existing_file {
                if !std::path::Path::new(&old_path).exists() {
                    // Old file no longer exists - this is a MOVE, update path
                    let _ = conn.execute(
                        "UPDATE files SET path = ?1, modified_at = ?2 WHERE id = ?3",
                        rusqlite::params![
                            file_result.file.path,
                            file_result.file.modified_at.to_rfc3339(),
                            file_id
                        ],
                    );

                    // Update INSTRUME based on destination root's unique_camera setting
                    // file_result.frame.instrume has the raw FITS header value (no suffix)
                    let new_instrume = if unique_camera {
                        file_result.frame.instrume.as_ref()
                            .map(|i| format!("{} N{}", i, root_id))
                    } else {
                        file_result.frame.instrume.clone()
                    };
                    let _ = conn.execute(
                        "UPDATE frames SET instrume = ?1 WHERE file_id = ?2",
                        rusqlite::params![new_instrume, file_id],
                    );

                    continue; // Skip insert, file was moved
                }
            }
        }

        // Insert file
        match insert_file(conn, &file_result.file) {
            Ok(file_id) => {
                // Update frame file_id in place (avoids clone of 32-field struct)
                file_result.frame.file_id = file_id;

                // Apply unique camera suffix to INSTRUME if enabled
                if unique_camera {
                    if let Some(ref instrume) = file_result.frame.instrume {
                        file_result.frame.instrume = Some(format!("{} N{}", instrume, root_id));
                    }
                }

                match insert_frame(conn, &file_result.frame) {
                    Ok(frame_id) => {
                        result.files_processed += 1;

                        // Track by image type
                        if let Some(ref imagetyp) = file_result.imagetyp {
                            match imagetyp {
                                ImageType::Light => lights_count += 1,
                                ImageType::Flat => flat_frame_ids.push(frame_id),
                                ImageType::Dark => dark_frame_ids.push(frame_id),
                                ImageType::Bias => bias_frame_ids.push(frame_id),
                                ImageType::DarkFlat => darkflat_frame_ids.push(frame_id),
                                ImageType::MasterDark => master_dark_ids.push(frame_id),
                                ImageType::MasterFlat => master_flat_ids.push(frame_id),
                                ImageType::MasterBias => master_bias_ids.push(frame_id),
                                ImageType::MasterDarkFlat => master_darkflat_ids.push(frame_id),
                                _ => {}
                            }
                        }

                        // Insert header if available
                        if let Some(ref header) = file_result.header {
                            let _ = insert_fits_header(conn, file_id, header);
                        }
                    }
                    Err(e) => {
                        errors.lock().unwrap().push(format!(
                            "{}: Failed to insert frame: {}",
                            file_result.file.path, e
                        ));
                    }
                }
            }
            Err(e) => {
                errors.lock().unwrap().push(format!(
                    "{}: Failed to insert file: {}",
                    file_result.file.path, e
                ));
            }
        }
    }

    // If the user cancelled mid-batch, ROLLBACK rather than commit a partial
    // transaction. Cancellation is supposed to mean "abort," not "keep what
    // I've partially inserted so far."
    if cancelled_mid_insert {
        crate::logging::log("INFO", "Phase 2: Cancelled mid-batch, rolling back transaction");
        if let Err(rb) = conn.execute("ROLLBACK", []) {
            crate::logging::log("ERROR", &format!("Phase 2: ROLLBACK after cancel failed: {}", rb));
            result.errors.push(format!("DB rollback after cancel failed: {}", rb));
        }
        // No COMMIT, no WAL checkpoint — caller still gets a populated result
        // (with cancelled=true) but the catalog is unchanged.
        result.errors = match Arc::try_unwrap(errors) {
            Ok(mutex) => mutex.into_inner().unwrap_or_default(),
            Err(arc) => arc.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        };
        emit_scan_complete(emitter, root_id, &result);
        return result;
    }

    // Commit transaction. If COMMIT fails, the transaction stays open on
    // the connection; ROLLBACK explicitly so it doesn't poison the pool.
    if let Err(e) = conn.execute("COMMIT", []) {
        crate::logging::log("ERROR", &format!("Phase 2: COMMIT failed: {}", e));
        result.errors.push(format!("DB commit failed: {}", e));
        if let Err(rb) = conn.execute("ROLLBACK", []) {
            crate::logging::log("ERROR", &format!("Phase 2: ROLLBACK after failed COMMIT failed: {}", rb));
        }
    }

    // Force WAL checkpoint to consolidate writes and reduce post-scan CPU activity
    // TRUNCATE mode moves all data from WAL to main DB and truncates the WAL file
    if let Err(e) = conn.execute("PRAGMA wal_checkpoint(TRUNCATE)", []) {
        crate::logging::log("WARN", &format!("Phase 2: WAL checkpoint failed: {}", e));
    }

    // Collect errors
    result.errors = match Arc::try_unwrap(errors) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc) => arc.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    };
    result.lights_count = lights_count;
    result.flats_count = flat_frame_ids.len();
    result.darks_count = dark_frame_ids.len();
    result.bias_count = bias_frame_ids.len();
    result.darkflats_count = darkflat_frame_ids.len();

    // Skip calibration and caching phases if cancelled
    if !result.cancelled {
        // Phase 3: Create calibration sets
        let has_calibration_frames = !flat_frame_ids.is_empty()
            || !dark_frame_ids.is_empty()
            || !bias_frame_ids.is_empty()
            || !darkflat_frame_ids.is_empty();

        let has_master_frames = !master_dark_ids.is_empty()
            || !master_flat_ids.is_empty()
            || !master_bias_ids.is_empty()
            || !master_darkflat_ids.is_empty();

        if has_calibration_frames || has_master_frames {
            emit_progress(emitter, root_id, 0, 0, None, "calibrating");

            use crate::calibration::scan_integration::{create_calibration_sets_from_scan_with_masters, MasterFrameIds};

            let master_frame_ids = MasterFrameIds {
                master_dark_ids,
                master_flat_ids,
                master_bias_ids,
                master_darkflat_ids,
            };

            match create_calibration_sets_from_scan_with_masters(
                conn,
                flat_frame_ids,
                dark_frame_ids,
                bias_frame_ids,
                darkflat_frame_ids,
                master_frame_ids,
            ) {
                Ok(cal_result) => {
                    result.calibration_sets_created = cal_result.sets_created as usize;
                }
                Err(e) => {
                    result.errors.push(format!("Failed to auto-create calibration sets: {}", e));
                }
            }
        }

        // Phase 4: Rebuild duplicate caches
        // This runs once after all scanning to pre-compute duplicate data
        emit_progress(
            emitter,
            root_id,
            result.files_processed,
            result.files_processed,
            None,
            "caching",
        );

        // Rebuild duplicate groups cache (using metadata hash by default)
        if let Err(e) = rebuild_duplicate_groups_cache(conn, false) {
            result.errors.push(format!("Failed to rebuild duplicate cache: {}", e));
        }

        // Rebuild folder similarity cache (threshold 70%)
        if let Err(e) = rebuild_folder_similarity_cache(conn, 70.0) {
            result.errors.push(format!("Failed to rebuild folder similarity cache: {}", e));
        }
    }

    // Emit completion
    emit_scan_complete(emitter, root_id, &result);

    result
}

/// Outcome of a registered scan — includes scan result and reconciliation info.
/// Returned by `run_registered_scan` so callers (Tauri command / monitor / web route)
/// can build their own DTOs and decide whether to recreate calibration sets.
pub struct RegisteredScanOutcome {
    pub result: ScanResult,
    pub reconcile: crate::db::ReconcileResult,
}

/// Run a scan against a scan root with full lifecycle management:
/// - Fails fast if a scan is already active for this root.
/// - Registers a `ScanHandle` in `ctx.active_scans` (with cancel flag).
/// - Reconciles `unique_camera` suffix state.
/// - Runs `scan_directory_parallel` with progress events via `emitter`.
/// - Persists `last_scan` timestamp and scan errors.
/// - Removes the `ScanHandle` from `active_scans` before returning.
///
/// This helper is the shared engine behind both the interactive Tauri
/// `start_scan_with_progress` command and the background monitor service.
/// It does NOT recreate calibration sets after reconciliation — that step is
/// interactive-only (monitor cycles never toggle `unique_camera`).
pub fn run_registered_scan<E: ProgressEmitter>(
    ctx: &crate::services::ServiceContext,
    emitter: &E,
    root_id: i64,
) -> Result<RegisteredScanOutcome, String> {
    use crate::services::ScanHandle;
    use std::sync::atomic::AtomicBool;

    // Atomically check-and-register so two concurrent calls cannot both
    // pass the "no scan in progress" check and both insert (previously the
    // mutex was released between contains_key and insert).
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut scans = ctx.active_scans.lock().unwrap();
        if scans.contains_key(&root_id) {
            return Err("Scan already in progress for this root".to_string());
        }
        scans.insert(
            root_id,
            ScanHandle { root_id, cancel_flag: cancel_flag.clone() },
        );
    }

    // Ensure the handle is removed on every exit path via a drop guard.
    struct ScanHandleGuard<'a> {
        ctx: &'a crate::services::ServiceContext,
        root_id: i64,
    }
    impl<'a> Drop for ScanHandleGuard<'a> {
        fn drop(&mut self) {
            let mut scans = self.ctx.active_scans.lock().unwrap();
            scans.remove(&self.root_id);
        }
    }
    let _guard = ScanHandleGuard { ctx, root_id };

    let db = ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Reconcile unique_camera state before scanning.
    let reconcile = crate::db::reconcile_unique_camera_instrume(&conn, root_id)
        .map_err(|e| format!("Reconciliation failed: {}", e))?;

    // Look up root config.
    let roots = crate::db::get_scan_roots(&conn).map_err(|e| e.to_string())?;
    let root = roots
        .into_iter()
        .find(|r| r.id == Some(root_id))
        .ok_or("Scan root not found")?;

    let use_content_hash = ctx
        .settings
        .get_duplicates_use_content_hash(&conn)
        .unwrap_or(false);

    let result = scan_directory_parallel(
        Path::new(&root.path),
        root_id,
        &conn,
        emitter,
        use_content_hash,
        cancel_flag,
        root.unique_camera,
    );

    // Persist last_scan timestamp.
    if let Err(e) = crate::db::update_scan_root_timestamp(&conn, root_id) {
        eprintln!("Failed to update scan timestamp for root {}: {}", root_id, e);
    }

    // Persist scan errors so they survive app restarts.
    if let Err(e) = crate::db::update_scan_root_errors(&conn, root_id, &result.errors) {
        eprintln!("Failed to persist scan errors: {}", e);
    }

    Ok(RegisteredScanOutcome { result, reconcile })
}
