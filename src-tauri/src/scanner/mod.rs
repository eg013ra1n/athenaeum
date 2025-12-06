// File scanner module
// Handles directory traversal and metadata extraction

use crate::db::{file_exists, insert_file, insert_frame, insert_fits_header};
use crate::fits_parser::{parse_fits, parse_xisf, extract_fits_header, extract_xisf_header};
use crate::duplicates::compute_metadata_hash;
use crate::models::{File, FileFormat, ImageType};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use walkdir::WalkDir;

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
                        _ => {} // Unknown types - no tracking
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

    if has_calibration_frames {
        match crate::calibration::scan_integration::create_calibration_sets_from_scan(
            conn,
            flat_frame_ids,
            dark_frame_ids,
            bias_frame_ids,
            darkflat_frame_ids,
        ) {
            Ok(cal_result) => {
                result.calibration_sets_created = cal_result.sets_created as usize;
                println!("Auto-created {} calibration sets from scan", cal_result.sets_created);
            }
            Err(e) => {
                println!("Warning: Failed to auto-create calibration sets: {}", e);
                // Non-fatal - frames are still in database
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

pub struct ScanProgress {
    pub current: usize,
    pub total: usize,
    pub current_file: Option<String>,
}
