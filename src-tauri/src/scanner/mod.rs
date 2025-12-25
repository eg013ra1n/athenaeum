// File scanner module
// Handles directory traversal and metadata extraction

use crate::db::{file_exists, insert_file, insert_frame, insert_fits_header};
use crate::fits_parser::{parse_fits, parse_xisf, extract_fits_header, extract_xisf_header};
use crate::duplicates::compute_metadata_hash;
use crate::models::{File, Frame, FileFormat, ImageType};
use chrono::Utc;
use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use walkdir::WalkDir;

/// Result from processing a single file (for collecting before batch insert)
#[derive(Clone)]
pub struct FileProcessResult {
    pub file: File,
    pub frame: Frame,
    pub header: Option<String>,
    pub imagetyp: Option<ImageType>,
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
}

/// Scan a directory for FITS/XISF files
pub fn scan_directory(
    root_path: &Path,
    conn: &Connection,
    progress_callback: Option<Box<dyn Fn(ScanProgress) + Send + Sync>>,
    use_content_hash: bool,
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
    };

    // Find all FITS/XISF files
    let files: Vec<PathBuf> = WalkDir::new(root_path)
        .follow_links(false)
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

        // Skip if already in database
        if file_exists(conn, &file_path.to_string_lossy()).unwrap_or(false) {
            *skipped.lock().unwrap() += 1;
            continue;
        }

        match process_file(file_path, conn, use_content_hash) {
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
    }

    result.files_processed = *processed.lock().unwrap();
    result.files_skipped = *skipped.lock().unwrap();
    result.errors = Arc::try_unwrap(errors).unwrap().into_inner().unwrap();

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
fn process_file(path: &PathBuf, conn: &Connection, use_content_hash: bool) -> anyhow::Result<Option<(i64, ImageType)>> {
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

    let current_path = path.to_string_lossy().to_string();

    // Check if file already exists at this exact path
    if file_exists(conn, &current_path)? {
        // File already indexed at this path - skip
        return Ok(None);
    }

    // Extract header early to check for moved files
    let header_result = match format {
        FileFormat::FITS => extract_fits_header(path),
        FileFormat::XISF => extract_xisf_header(path),
    };

    if let Ok(header) = header_result {
        let fingerprint = crate::fingerprint::compute_header_fingerprint(&header);

        // Check if a file with this fingerprint exists at a different path
        let existing_file: Option<(i64, String)> = conn.query_row(
            "SELECT f.id, f.path FROM files f
             INNER JOIN fits_header fh ON f.id = fh.file_id
             WHERE fh.header_fingerprint = ?1 AND f.path != ?2",
            rusqlite::params![fingerprint, current_path],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;

        if let Some((file_id, old_path)) = existing_file {
            // Check if the old file still exists on disk
            if std::path::Path::new(&old_path).exists() {
                // Old file still exists - this is a DUPLICATE, not a move
                // Continue with normal insertion to create a separate record
                println!("Detected duplicate file: '{}' and '{}' have identical headers", old_path, current_path);
            } else {
                // Old file no longer exists - this is a MOVE
                println!("Detected moved file: '{}' -> '{}' (file_id={})", old_path, current_path, file_id);

                conn.execute(
                    "UPDATE files SET path = ?1, modified_at = ?2 WHERE id = ?3",
                    rusqlite::params![current_path, modified_dt.to_rfc3339(), file_id],
                )?;

                // File metadata and header already exist, no need to re-insert
                return Ok(None);
            }
        }
    }

    // If we get here, it's a truly new file - insert it
    let metadata_hash = compute_metadata_hash(size, &modified_dt, &filename);

    // Compute content hash if enabled in settings
    let content_hash = if use_content_hash {
        match crate::duplicates::compute_xxhash(path) {
            Ok(hash) => {
                println!("Computed content hash for '{}': {}", current_path, hash);
                Some(hash)
            }
            Err(e) => {
                println!("Warning: Failed to compute content hash for '{}': {}", current_path, e);
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

    // Parse and insert frame metadata
    let frame = match format {
        FileFormat::FITS => parse_fits(path, file_id)?,
        FileFormat::XISF => parse_xisf(path, file_id)?,
    };

    let frame_id = insert_frame(conn, &frame)?;
    let imagetyp = frame.imagetyp.clone();

    // Store full FITS header for future reference
    if format == FileFormat::FITS {
        match extract_fits_header(path) {
            Ok(header) => {
                println!("Storing FITS header for file_id={}, header length={} bytes", file_id, header.len());
                if let Err(e) = insert_fits_header(conn, file_id, &header) {
                    println!("Warning: Failed to store FITS header: {}", e);
                    // Continue processing - header storage is non-critical
                }
            }
            Err(e) => {
                println!("Warning: Failed to extract FITS header: {}", e);
                // Continue processing - header storage is non-critical
            }
        }
    } else if format == FileFormat::XISF {
        // Store XISF header for future reference
        match extract_xisf_header(path) {
            Ok(header) => {
                println!("Storing XISF header for file_id={}, header length={} bytes", file_id, header.len());
                if let Err(e) = insert_fits_header(conn, file_id, &header) {
                    println!("Warning: Failed to store XISF header: {}", e);
                    // Continue processing - header storage is non-critical
                }
            }
            Err(e) => {
                println!("Warning: Failed to extract XISF header: {}", e);
                // Continue processing - header storage is non-critical
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
}

#[allow(dead_code)]
pub struct ScanProgress {
    pub current: usize,
    pub total: usize,
    pub current_file: Option<String>,
}

/// Emit progress event to frontend (throttled)
fn emit_progress(
    app_handle: &tauri::AppHandle,
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

    let _ = app_handle.emit("scan-progress", &event);
}

/// Emit scan complete event to frontend
fn emit_scan_complete(app_handle: &tauri::AppHandle, root_id: i64, result: &ScanResult) {
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
    };

    let _ = app_handle.emit("scan-complete", &event);
}

/// Process a single file without database access (safe for parallel execution)
/// Returns file data ready for batch insertion
fn process_file_parallel(
    path: &PathBuf,
    use_content_hash: bool,
) -> Result<FileProcessResult, String> {
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

    let current_path = path.to_string_lossy().to_string();

    // Compute metadata hash
    let metadata_hash = compute_metadata_hash(size, &modified_dt, &filename);

    // Compute content hash if enabled
    let content_hash = if use_content_hash {
        crate::duplicates::compute_xxhash(path).ok()
    } else {
        None
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

    // Parse FITS/XISF metadata (using file_id=0 as placeholder, will be updated during insert)
    let frame = match format {
        FileFormat::FITS => parse_fits(path, 0).map_err(|e| e.to_string())?,
        FileFormat::XISF => parse_xisf(path, 0).map_err(|e| e.to_string())?,
    };

    // Extract header
    let header = match format {
        FileFormat::FITS => extract_fits_header(path).ok(),
        FileFormat::XISF => extract_xisf_header(path).ok(),
    };

    let imagetyp = frame.imagetyp.clone();

    Ok(FileProcessResult {
        file,
        frame,
        header,
        imagetyp,
    })
}

/// Parallel scan a directory for FITS/XISF files with progress reporting
///
/// This function uses a two-phase approach:
/// - Phase 1: Parallel file discovery and metadata extraction (CPU-bound)
/// - Phase 2: Sequential database inserts in a single transaction (I/O-bound)
pub fn scan_directory_parallel(
    root_path: &Path,
    root_id: i64,
    conn: &Connection,
    app_handle: &tauri::AppHandle,
    use_content_hash: bool,
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
    };

    // Phase 1a: Discovery - collect all file paths
    emit_progress(app_handle, root_id, 0, 0, None, "discovery");

    let files: Vec<PathBuf> = WalkDir::new(root_path)
        .follow_links(false)
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

    if files.is_empty() {
        emit_scan_complete(app_handle, root_id, &result);
        return result;
    }

    // Build a set of existing file paths for quick lookup
    let existing_paths: std::collections::HashSet<String> = {
        let mut paths = std::collections::HashSet::new();
        let mut stmt = conn.prepare("SELECT path FROM files").unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
        for path in rows.flatten() {
            paths.insert(path);
        }
        paths
    };

    // Filter out files that already exist in database
    let new_files: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| !existing_paths.contains(&p.to_string_lossy().to_string()))
        .collect();

    result.files_skipped = result.files_found - new_files.len();

    if new_files.is_empty() {
        emit_scan_complete(app_handle, root_id, &result);
        return result;
    }

    // Phase 1b: Parallel processing - extract metadata from all files
    let progress_counter = Arc::new(AtomicUsize::new(0));
    let total_new = new_files.len();
    let errors = Arc::new(Mutex::new(Vec::new()));
    let app_handle_clone = app_handle.clone();

    let processed_results: Vec<FileProcessResult> = new_files
        .par_iter()
        .filter_map(|path| {
            let current = progress_counter.fetch_add(1, Ordering::SeqCst) + 1;

            // Emit progress every 10 files or on last file
            if current % 10 == 0 || current == total_new {
                emit_progress(
                    &app_handle_clone,
                    root_id,
                    current,
                    total_new,
                    path.file_name().map(|n| n.to_string_lossy().to_string()),
                    "processing",
                );
            }

            match process_file_parallel(path, use_content_hash) {
                Ok(result) => Some(result),
                Err(e) => {
                    errors
                        .lock()
                        .unwrap()
                        .push(format!("{}: {}", path.display(), e));
                    None
                }
            }
        })
        .collect();

    // Phase 2: Sequential database inserts in a transaction
    emit_progress(app_handle, root_id, 0, processed_results.len(), None, "inserting");

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
    let _ = conn.execute("BEGIN TRANSACTION", []);

    for (idx, file_result) in processed_results.iter().enumerate() {
        // Emit progress every 50 files during insert phase
        if idx % 50 == 0 || idx == processed_results.len() - 1 {
            emit_progress(
                app_handle,
                root_id,
                idx + 1,
                processed_results.len(),
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
                    continue; // Skip insert, file was moved
                }
            }
        }

        // Insert file
        match insert_file(conn, &file_result.file) {
            Ok(file_id) => {
                // Update frame with correct file_id and insert
                let mut frame = file_result.frame.clone();
                frame.file_id = file_id;

                match insert_frame(conn, &frame) {
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

    // Commit transaction
    let _ = conn.execute("COMMIT", []);

    // Collect errors
    result.errors = Arc::try_unwrap(errors).unwrap().into_inner().unwrap();
    result.lights_count = lights_count;
    result.flats_count = flat_frame_ids.len();
    result.darks_count = dark_frame_ids.len();
    result.bias_count = bias_frame_ids.len();
    result.darkflats_count = darkflat_frame_ids.len();

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
        emit_progress(app_handle, root_id, 0, 0, None, "calibrating");

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

    // Emit completion
    emit_scan_complete(app_handle, root_id, &result);

    result
}
