// File scanner module
// Handles directory traversal and metadata extraction

use crate::db::{file_exists, insert_file, insert_frame};
use crate::duplicates::compute_xxhash;
use crate::fits_parser::{parse_fits, parse_xisf};
use crate::models::{File, FileFormat};
use chrono::Utc;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use walkdir::WalkDir;

/// Scan a directory for FITS/XISF files
pub fn scan_directory(
    root_path: &Path,
    conn: &Connection,
    progress_callback: Option<Box<dyn Fn(ScanProgress) + Send + Sync>>,
) -> ScanResult {
    let mut result = ScanResult {
        files_found: 0,
        files_processed: 0,
        files_skipped: 0,
        errors: Vec::new(),
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

        match process_file(file_path, conn) {
            Ok(_) => {
                *processed.lock().unwrap() += 1;
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

    result
}

/// Process a single file: hash, parse metadata, insert into database
fn process_file(path: &PathBuf, conn: &Connection) -> anyhow::Result<()> {
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

    // Compute hash
    let content_hash = compute_xxhash(path)?;

    // Insert file record
    let file = File {
        id: None,
        path: path.to_string_lossy().to_string(),
        size,
        modified_at: modified_dt,
        format: format.clone(),
        content_hash,
        duplicate_group_id: None,
        created_at: Utc::now(),
    };

    let file_id = insert_file(conn, &file)?;

    // Parse and insert frame metadata
    let frame = match format {
        FileFormat::FITS => parse_fits(path, file_id)?,
        FileFormat::XISF => parse_xisf(path, file_id)?,
    };

    insert_frame(conn, &frame)?;

    Ok(())
}

pub struct ScanResult {
    pub files_found: usize,
    pub files_processed: usize,
    pub files_skipped: usize,
    pub errors: Vec<String>,
}

pub struct ScanProgress {
    pub current: usize,
    pub total: usize,
    pub current_file: Option<String>,
}
