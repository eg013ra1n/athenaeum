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
use anyhow::Context;
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

/// True when `path` is `root` itself, or a descendant of `root` at a
/// path-separator boundary. Root `/a/b` matches `/a/b/x.fits` but NOT the
/// sibling `/a/bc/x.fits` (a plain string-prefix check would wrongly match
/// the sibling).
///
/// `root` is normalized by trimming trailing `/`/`\` before comparing —
/// without this, a root stored with a trailing separator (Windows drive
/// roots like `D:\`, or a legacy/manually-edited `scan_roots` row ending in
/// `/`) would never match any of its own descendants: `strip_prefix` would
/// consume the separator as part of the literal prefix, leaving a `rest`
/// that doesn't itself start with a separator. Degenerate case: root `/`
/// trims to `""`, so every absolute unix path matches it — a root of `/`
/// legitimately owns the whole filesystem.
fn path_has_root_prefix(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches(['/', '\\']);
    if path == root {
        return true;
    }
    path.strip_prefix(root)
        .map(|rest| rest.starts_with('/') || rest.starts_with('\\'))
        .unwrap_or(false)
}

/// If `path` falls under a scan root whose root directory is currently
/// missing on disk (volume unmounted / disconnected), returns that root's
/// path. Used to stop the header-fingerprint move-detection from mistaking
/// a duplicate/copy on offline storage for a move: flipping `files.path`
/// away from a still-valid-but-offline original would silently orphan it
/// once the volume remounts (project rule: files on disconnected storage
/// are not orphans).
///
/// When multiple configured scan roots match `path` by prefix (nested
/// roots), the longest (most specific) match wins. Not under any known
/// scan root, or under one that's available -> `Ok(None)` (today's
/// behavior, unaffected by this guard).
fn path_under_unavailable_scan_root(
    conn: &Connection,
    path: &str,
) -> anyhow::Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT path FROM scan_roots")?;
    let root_paths: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let owning_root = root_paths
        .into_iter()
        .filter(|root| path_has_root_prefix(path, root))
        .max_by_key(|root| root.len());

    Ok(owning_root.filter(|root| !std::path::Path::new(root).exists()))
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
        // If size or modified_at changed, re-parse and UPDATE in place so
        // files.id and frames.id are preserved — junction tables
        // (session_members, calibration_set_frames, frame_tags) survive
        // the rescan. The previous DELETE-and-reinsert path silently
        // orphaned session memberships any time mtime drifted.
        let path_str = file_path.to_string_lossy().to_string();
        let existing = conn
            .query_row(
                "SELECT id, size, modified_at FROM files WHERE path = ?1",
                rusqlite::params![path_str],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?)),
            )
            .ok();
        if let Some((file_id, db_size, db_modified)) = existing {
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
            // Modified in place — re-parse and UPDATE existing rows. On
            // parse failure, leave the catalog row untouched so the
            // bookkeeping (junction tables, calibration links) stays
            // valid; just record the error and move on.
            match reparse_and_update_in_place(
                file_path, file_id, conn, use_content_hash, unique_camera, root_id,
            ) {
                Ok(()) => {
                    *processed.lock().unwrap() += 1;
                }
                Err(e) => {
                    errors.lock().unwrap().push(format!(
                        "{}: failed to re-parse modified file in place: {}",
                        file_path.display(),
                        e
                    ));
                }
            }
            continue;
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
                tracing::info!(
                    root_id,
                    count = cal_result.sets_created,
                    master_count = master_total,
                    "auto-created calibration sets from scan"
                );
            }
            Err(e) => {
                // Surface errors to user instead of just logging
                result.errors.push(format!("Failed to auto-create calibration sets: {}", e));
                tracing::error!(root_id, error = %e, "failed to auto-create calibration sets");
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
                tracing::debug!(
                    root_id,
                    old_path = %old_path,
                    path = %current_path,
                    "duplicate file detected (identical header)"
                );
            } else if let Some(offline_root) = path_under_unavailable_scan_root(conn, &old_path)? {
                // Old path is missing, but that's because its owning scan
                // root is currently offline (unmounted volume), not because
                // the file was moved. Fall through to duplicate/new-file
                // handling below instead of flipping the still-valid
                // original's path.
                tracing::warn!(
                    root_id,
                    old_path = %old_path,
                    path = %current_path,
                    offline_root = %offline_root,
                    file_id,
                    "skipping move-detection: old path's scan root is unavailable"
                );
            } else {
                tracing::debug!(
                    root_id,
                    old_path = %old_path,
                    path = %current_path,
                    file_id,
                    "moved file detected"
                );

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
                conn.execute(
                    "UPDATE frames SET instrume = ?1 WHERE file_id = ?2",
                    rusqlite::params![new_instrume, file_id],
                ).map_err(|e| {
                    tracing::error!(
                        root_id,
                        file_id,
                        old_path = %old_path,
                        path = %current_path,
                        error = %e,
                        "failed to update frame instrume after move"
                    );
                    e
                })?;

                return Ok(None);
            }
        }
    }

    // If we get here, it's a truly new file - insert it
    let metadata_hash = compute_metadata_hash(size, &modified_dt, &filename);

    let content_hash = if use_content_hash {
        match crate::duplicates::compute_xxhash(path) {
            Ok(hash) => {
                tracing::debug!(root_id, path = %current_path, hash = %hash, "computed content hash");
                Some(hash)
            }
            Err(e) => {
                // Surface the failure so the user knows duplicate detection
                // skipped this file. Previously silently dropped to None.
                let msg = format!("hash_error: {}: failed to compute content hash: {}", current_path, e);
                tracing::warn!(path = %current_path, error = %e, "failed to compute content hash");
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
        archived_in_operation: None,
        archive_zip_path: None,
        archive_path_in_zip: None,
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
        tracing::debug!(root_id, file_id, header_len = header.len(), "storing fits header");
        if let Err(e) = insert_fits_header(conn, file_id, &header) {
            tracing::error!(root_id, file_id, error = %e, "failed to store fits header");
        }
    } else if format == FileFormat::XISF {
        // XISF header extraction failed earlier, try again
        match extract_xisf_header(path) {
            Ok(header) => {
                tracing::debug!(root_id, file_id, header_len = header.len(), "storing xisf header");
                if let Err(e) = insert_fits_header(conn, file_id, &header) {
                    tracing::error!(root_id, file_id, error = %e, "failed to store xisf header");
                }
            }
            Err(e) => {
                tracing::error!(root_id, file_id, error = %e, "failed to extract xisf header");
            }
        }
    }

    // Return frame info if imagetyp is known
    Ok(imagetyp.map(|it| (frame_id, it)))
}

/// Re-parse a file whose on-disk metadata has drifted from the catalog and
/// UPDATE the existing files / frames / fits_header rows in place. Preserves
/// files.id and frames.id so every junction-table linkage (session_members,
/// calibration_set_frames, calibration_set_to_frames, frame_tags) survives.
///
/// Returns Ok(()) on success. On parse failure, leaves the DB row untouched
/// and returns Err — the caller should log it as a non-fatal error so the
/// rest of the scan continues.
fn reparse_and_update_in_place(
    path: &PathBuf,
    file_id: i64,
    conn: &Connection,
    use_content_hash: bool,
    unique_camera: bool,
    root_id: i64,
) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(path)?;
    let size = metadata.len() as i64;
    let modified_dt = chrono::DateTime::<Utc>::from(metadata.modified()?);

    let format = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .map(|e| if e == "xisf" { FileFormat::XISF } else { FileFormat::FITS })
        .unwrap_or(FileFormat::FITS);
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    // Parse FIRST. If parse fails, we leave the DB alone — better stale
    // than missing.
    let (frame, header_text) = match format {
        FileFormat::FITS => {
            let (f, h) = parse_fits_with_header(path, file_id)?;
            (f, Some(h))
        }
        FileFormat::XISF => {
            let f = parse_xisf(path, file_id)?;
            let h = extract_xisf_header(path).ok();
            (f, h)
        }
    };

    let metadata_hash = compute_metadata_hash(size, &modified_dt, &filename);
    let content_hash = if use_content_hash {
        Some(
            crate::duplicates::compute_xxhash(path)
                .with_context(|| format!("compute content hash for {}", path.display()))?,
        )
    } else {
        None
    };

    let new_instrume = if unique_camera {
        frame.instrume.as_ref().map(|i| {
            // Strip a previous " N<digits>" suffix before re-applying so we
            // don't accumulate "N1 N1 N1" on repeated rescans.
            let base = if let Some(pos) = i.rfind(" N") {
                let suffix = &i[pos + 2..];
                if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                    &i[..pos]
                } else {
                    i.as_str()
                }
            } else {
                i.as_str()
            };
            format!("{} N{}", base, root_id)
        })
    } else {
        frame.instrume.clone()
    };

    // Defensive: ensure no more than one frames row points at this file_id.
    // frame_count == 1 → UPDATE in place (the common case).
    // frame_count == 0 → INSERT a fresh frames row. This recovers from an
    //                    earlier scan that inserted a `files` row but failed
    //                    to parse the FITS, leaving an orphaned files row
    //                    with no `frames`. Without this branch, that orphan
    //                    stays stuck because subsequent scans classify the
    //                    file as "unchanged" (mtime matches) and the
    //                    "new file" branch never runs (the files row exists).
    // frame_count > 1  → bail; we don't know which row to update.
    let frame_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM frames WHERE file_id = ?1",
        rusqlite::params![file_id],
        |r| r.get(0),
    )?;
    if frame_count > 1 {
        anyhow::bail!(
            "expected at most 1 frames row for file_id={}, found {}",
            file_id,
            frame_count,
        );
    }

    // Has the user manually edited this frame's metadata? frames.override = 1
    // means "user has edited; scanner must not undo". The frames row is
    // authoritative in that case: skip the frames UPDATE entirely and only
    // refresh the files row (size/mtime/hashes) + the stored header snapshot,
    // so the file stops being classified as "modified" without wiping the
    // user's edits.
    let user_override: bool = if frame_count == 1 {
        conn.query_row(
            "SELECT override FROM frames WHERE file_id = ?1",
            rusqlite::params![file_id],
            |r| r.get::<_, i64>(0),
        )? != 0
    } else {
        false
    };

    // Wrap all DB writes in a SAVEPOINT so a process kill / error mid-update
    // can't leave a half-updated row (e.g. files synced but frames stale, or
    // fits_header deleted but not re-inserted). A SAVEPOINT — not BEGIN —
    // because scan_directory_parallel calls this function while its own
    // batch transaction is open, and a nested BEGIN is a SQLite error
    // ("cannot start a transaction within a transaction"). SAVEPOINT nests
    // inside an open transaction and acts like a regular transaction when
    // none is open (the serial scan path). The pre-parse work above
    // (metadata stat, format detection, FITS parse, hash computation,
    // frame_count/override reads) is read-only and intentionally stays
    // OUTSIDE the savepoint.
    conn.execute_batch("SAVEPOINT reparse_in_place")?;
    let write_result = write_reparse_rows(
        conn,
        file_id,
        size,
        &modified_dt,
        &format,
        &metadata_hash,
        content_hash.as_deref(),
        &frame,
        new_instrume.as_deref(),
        header_text.as_deref(),
        frame_count,
        user_override,
    );
    match write_result {
        Ok(()) => {
            conn.execute_batch("RELEASE reparse_in_place")?;
            Ok(())
        }
        Err(e) => {
            if let Err(rb) = conn.execute_batch(
                "ROLLBACK TO reparse_in_place; RELEASE reparse_in_place",
            ) {
                tracing::error!(path = %path.display(), error = %rb, "savepoint rollback failed");
            }
            Err(e)
        }
    }
}

/// The write half of `reparse_and_update_in_place`. Runs inside the
/// `reparse_in_place` savepoint opened by the caller — every statement here
/// either all commits (RELEASE) or all rolls back (ROLLBACK TO).
#[allow(clippy::too_many_arguments)]
fn write_reparse_rows(
    conn: &Connection,
    file_id: i64,
    size: i64,
    modified_dt: &chrono::DateTime<Utc>,
    format: &FileFormat,
    metadata_hash: &str,
    content_hash: Option<&str>,
    frame: &Frame,
    new_instrume: Option<&str>,
    header_text: Option<&str>,
    frame_count: i64,
    user_override: bool,
) -> anyhow::Result<()> {
    // UPDATE files in place. Mirrors insert_file's column list (path,
    // filename, created_at intentionally not touched).
    conn.execute(
        "UPDATE files
         SET size = ?1, modified_at = ?2, format = ?3,
             metadata_hash = ?4, content_hash = ?5
         WHERE id = ?6",
        rusqlite::params![
            size,
            modified_dt.to_rfc3339(),
            format!("{:?}", format),
            Some(metadata_hash),
            content_hash,
            file_id,
        ],
    )?;

    // UPDATE frames in place if a row exists; otherwise INSERT one. The
    // INSERT branch handles the case where a previous scan inserted the
    // `files` row but failed to parse the FITS — leaving an orphaned files
    // row with no `frames`. Without this branch, that orphan stays stuck
    // because the file would now be classified as "unchanged" on every
    // subsequent scan. When the user has edited the frame (override = 1)
    // the frames row is left completely untouched.
    if user_override {
        // Intentionally no frames UPDATE: user edits win over header values.
    } else if frame_count == 1 {
        // Mirrors insert_frame's column list. Note that imagetyp is stored
        // as the Debug form of ImageType (matches insert_frame),
        // is_master/override are written as i64 booleans, and bayerpat is
        // included (added via migration; see schema.rs).
        let imagetyp_str = frame.imagetyp.as_ref().map(|t| format!("{:?}", t));
        let date_obs_str = frame.date_obs.as_ref().map(|d| d.to_rfc3339());
        let is_master_int = if frame.is_master { 1i64 } else { 0i64 };
        let override_int = if frame.override_ { 1i64 } else { 0i64 };
        conn.execute(
            "UPDATE frames SET
                object = ?1, date_obs = ?2, telescop = ?3, instrume = ?4,
                exptime = ?5, filter = ?6, imagetyp = ?7, is_master = ?8,
                gain = ?9, offset = ?10, binning = ?11, xbinning = ?12,
                ybinning = ?13, ccd_temp = ?14, set_temp = ?15, focallen = ?16,
                xpixsz = ?17, ypixsz = ?18, naxis1 = ?19, naxis2 = ?20,
                ra = ?21, dec = ?22, sitelat = ?23, lat_obs = ?24,
                sitelong = ?25, long_obs = ?26, objctra = ?27, objctdec = ?28,
                override = ?29, swcreate = ?30, bayerpat = ?31, rotation = ?32
             WHERE file_id = ?33",
            rusqlite::params![
                frame.object,
                date_obs_str,
                frame.telescop,
                new_instrume,
                frame.exptime,
                frame.filter,
                imagetyp_str,
                is_master_int,
                frame.gain,
                frame.offset,
                frame.binning,
                frame.xbinning,
                frame.ybinning,
                frame.ccd_temp,
                frame.set_temp,
                frame.focallen,
                frame.xpixsz,
                frame.ypixsz,
                frame.naxis1,
                frame.naxis2,
                frame.ra,
                frame.dec,
                frame.sitelat,
                frame.lat_obs,
                frame.sitelong,
                frame.long_obs,
                frame.objctra,
                frame.objctdec,
                override_int,
                frame.swcreate,
                frame.bayerpat,
                frame.rotation,
                file_id,
            ],
        )?;
    } else {
        // frame_count == 0: insert a new frames row pointing at file_id.
        // Build a Frame with the correct file_id and the unique-camera
        // suffix applied (so both the UPDATE and INSERT branches share the
        // same instrume substitution logic), leave id = None so the
        // canonical insert allocates a fresh auto-increment id. The write
        // stays inside the caller's savepoint.
        let mut frame_for_insert = frame.clone();
        frame_for_insert.id = None;
        frame_for_insert.file_id = file_id;
        frame_for_insert.instrume = new_instrume.map(|s| s.to_string());
        insert_frame(conn, &frame_for_insert)?;
    }

    // fits_header has NO UNIQUE(file_id) constraint, so the DELETE-then-INSERT
    // is what keeps it to one row per file — do not drop the DELETE or this
    // silently duplicates header rows. The re-INSERT also refreshes
    // header_fingerprint to reflect the new bytes. No FK references fits_header
    // rows, so this is safe. Done even when override = 1: the snapshot must
    // reflect the current on-disk bytes (move detection + metadata-pane revert
    // both read it).
    if let Some(h) = header_text {
        let fingerprint = crate::fingerprint::compute_header_fingerprint(h);
        conn.execute(
            "DELETE FROM fits_header WHERE file_id = ?1",
            rusqlite::params![file_id],
        )?;
        conn.execute(
            "INSERT INTO fits_header (file_id, header, header_fingerprint)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![file_id, h, fingerprint],
        )?;
    }

    Ok(())
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
    root_id: i64,
) -> Result<FileProcessResult, String> {
    tracing::debug!(root_id, path = %path.display(), "processing file");

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
        archived_in_operation: None,
        archive_zip_path: None,
        archive_path_in_zip: None,
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
    tracing::info!(path = %root_path.display(), stage = "discovery", "starting file discovery");
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
    tracing::info!(count = files.len(), stage = "discovery", "file discovery complete");

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

    // Build a map of existing file paths to their stored (id, size,
    // modified_at). We use this to classify each on-disk file as new /
    // unchanged / modified so that in-place modifications get re-parsed
    // instead of silently skipped (the previous path-only filter).
    // modified_at is stored as RFC3339 in the DB and compared as a string
    // for an exact-match check. The `id` is carried through so the write
    // loop below can dispatch UPDATE-in-place without a per-file
    // `SELECT id FROM files` (N+1) query.
    tracing::debug!(root_id, "building existing files map from DB");
    let existing_files: std::collections::HashMap<String, (i64, i64, String)> = {
        let mut map = std::collections::HashMap::new();
        match conn.prepare("SELECT path, id, size, modified_at FROM files") {
            Ok(mut stmt) => {
                match stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                }) {
                    Ok(rows) => {
                        for entry in rows.flatten() {
                            map.insert(entry.0, (entry.1, entry.2, entry.3));
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to query existing files");
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to prepare existing files query");
            }
        }
        map
    };

    // Classification: each on-disk file is either NEW (no DB row) or
    // MODIFIED (DB row exists but size/mtime drifted). Unchanged files are
    // dropped here. The sequential write phase below dispatches NEW vs
    // MODIFIED separately (INSERT vs UPDATE-in-place). For modified files
    // we carry the `file_id` along with the path so the write loop never
    // needs to re-query it.
    let mut modified_paths: Vec<(String, i64)> = Vec::new();
    let new_files: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| {
            let path_str = p.to_string_lossy().to_string();
            match existing_files.get(&path_str) {
                None => true,
                Some((db_id, db_size, db_modified)) => {
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
                                modified_paths.push((path_str, *db_id));
                                true
                            }
                        }
                        Err(_) => false,
                    }
                }
            }
        })
        .collect();

    result.files_skipped = result.files_found - new_files.len();
    tracing::info!(
        count = new_files.len(),
        unchanged = result.files_skipped,
        modified = modified_paths.len(),
        "files to process (modified will UPDATE in place)"
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
    tracing::info!(count = new_files.len(), stage = "processing", "starting parallel FITS parsing");
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
                process_file_parallel(path, use_content_hash, root_id)
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

    tracing::info!(count = processed_results.len(), stage = "processing", "parallel FITS parsing complete");

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
    tracing::debug!(root_id, count = processed_results.len(), stage = "inserting", "starting DB inserts");
    if let Err(e) = conn.execute("BEGIN TRANSACTION", []) {
        tracing::error!(error = %e, stage = "inserting", "begin transaction failed");
        result.errors.push(format!("Failed to start DB transaction: {}", e));
        emit_scan_complete(emitter, root_id, &result);
        return result;
    }

    // Path -> file_id lookup for the write-loop's UPDATE-in-place branch.
    // Built from the classification phase's `modified_paths` so we don't
    // need a per-file `SELECT id FROM files` (N+1) query.
    let modified_files_by_path: std::collections::HashMap<String, i64> =
        modified_paths.iter().cloned().collect();

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

            let existing_file: Option<(i64, String)> = match conn.query_row(
                "SELECT f.id, f.path FROM files f
                 INNER JOIN fits_header fh ON f.id = fh.file_id
                 WHERE fh.header_fingerprint = ?1 AND f.path != ?2",
                rusqlite::params![fingerprint, file_result.file.path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional() {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        root_id,
                        path = %file_result.file.path,
                        error = %e,
                        "move-detection fingerprint lookup failed"
                    );
                    result.errors.push(format!(
                        "{}: move-detection fingerprint lookup failed: {}",
                        file_result.file.path, e
                    ));
                    None
                }
            };

            if let Some((file_id, old_path)) = existing_file {
                let old_path_missing = !std::path::Path::new(&old_path).exists();
                // Only consult the volume guard when we'd otherwise treat
                // this as a move — same "old path missing" branch as before.
                let guard_blocking_root: Option<String> = if old_path_missing {
                    match path_under_unavailable_scan_root(conn, &old_path) {
                        Ok(root) => root,
                        Err(e) => {
                            tracing::error!(
                                root_id,
                                old_path = %old_path,
                                error = %e,
                                "move-detection volume guard check failed"
                            );
                            result.errors.push(format!(
                                "{}: move-detection volume guard check failed: {}",
                                old_path, e
                            ));
                            None
                        }
                    }
                } else {
                    None
                };

                if let Some(offline_root) = guard_blocking_root {
                    // Old path is missing, but only because its owning scan
                    // root is currently offline (unmounted volume). Fall
                    // through to the normal insert/reparse handling below
                    // instead of flipping the still-valid original's path.
                    tracing::warn!(
                        root_id,
                        old_path = %old_path,
                        path = %file_result.file.path,
                        offline_root = %offline_root,
                        file_id,
                        "skipping move-detection: old path's scan root is unavailable"
                    );
                } else if old_path_missing {
                    // Old file no longer exists - this is a MOVE, update path
                    if let Err(e) = conn.execute(
                        "UPDATE files SET path = ?1, modified_at = ?2 WHERE id = ?3",
                        rusqlite::params![
                            file_result.file.path,
                            file_result.file.modified_at.to_rfc3339(),
                            file_id
                        ],
                    ) {
                        tracing::error!(
                            root_id,
                            file_id,
                            old_path = %old_path,
                            path = %file_result.file.path,
                            error = %e,
                            "failed to update files.path after move"
                        );
                        result.errors.push(format!(
                            "file_id={}: failed to update path for moved file '{}' -> '{}': {}",
                            file_id, old_path, file_result.file.path, e
                        ));
                    }

                    // Update INSTRUME based on destination root's unique_camera setting
                    // file_result.frame.instrume has the raw FITS header value (no suffix)
                    let new_instrume = if unique_camera {
                        file_result.frame.instrume.as_ref()
                            .map(|i| format!("{} N{}", i, root_id))
                    } else {
                        file_result.frame.instrume.clone()
                    };
                    if let Err(e) = conn.execute(
                        "UPDATE frames SET instrume = ?1 WHERE file_id = ?2",
                        rusqlite::params![new_instrume, file_id],
                    ) {
                        tracing::error!(
                            root_id,
                            file_id,
                            old_path = %old_path,
                            path = %file_result.file.path,
                            error = %e,
                            "failed to update frame instrume after move"
                        );
                        result.errors.push(format!(
                            "file_id={}: failed to update instrume for moved file '{}' -> '{}': {}",
                            file_id, old_path, file_result.file.path, e
                        ));
                    }

                    continue; // Skip insert, file was moved
                }
            }
        }

        // If this file is a modified existing row (not a new file), use the
        // non-destructive in-place UPDATE path. Preserves files.id +
        // frames.id and therefore every junction-table linkage. The
        // `file_id` is plumbed through from the classification phase, so
        // no per-row SELECT is needed here.
        if let Some(&file_id) = modified_files_by_path.get(&file_result.file.path) {
            let path_buf = PathBuf::from(&file_result.file.path);
            match reparse_and_update_in_place(
                &path_buf, file_id, conn, use_content_hash, unique_camera, root_id,
            ) {
                Ok(()) => {
                    result.files_processed += 1;
                    // Track image type for calibration set creation, BUT
                    // intentionally do NOT push to flat_frame_ids /
                    // dark_frame_ids / etc. — those drive
                    // create_calibration_sets_from_scan_with_masters which
                    // would create a duplicate calibration set if we did.
                    // The original calibration_set_frames row already
                    // points at frames.id (preserved) and stays valid.
                    if let Some(ref imagetyp) = file_result.imagetyp {
                        if matches!(imagetyp, ImageType::Light) {
                            lights_count += 1;
                        }
                    }
                    continue;
                }
                Err(e) => {
                    result.errors.push(format!(
                        "{}: in-place re-parse failed: {}",
                        file_result.file.path, e,
                    ));
                    continue;
                }
            }
        }

        // New file path — existing INSERT behavior.
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
        tracing::info!(stage = "inserting", outcome = "cancelled", "rolling back transaction after cancel");
        if let Err(rb) = conn.execute("ROLLBACK", []) {
            tracing::error!(error = %rb, stage = "inserting", "rollback after cancel failed");
            result.errors.push(format!("DB rollback after cancel failed: {}", rb));
        }
        // No COMMIT, no WAL checkpoint — caller still gets a populated result
        // (with cancelled=true) but the catalog is unchanged. EXTEND, don't
        // assign: result.errors already holds write-loop errors (in-place
        // re-parse failures, rollback failures) that an assignment would
        // silently discard.
        let phase1_errors = match Arc::try_unwrap(errors) {
            Ok(mutex) => mutex.into_inner().unwrap_or_default(),
            Err(arc) => arc.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        };
        result.errors.extend(phase1_errors);
        emit_scan_complete(emitter, root_id, &result);
        return result;
    }

    // Commit transaction. If COMMIT fails, the transaction stays open on
    // the connection; ROLLBACK explicitly so it doesn't poison the pool.
    if let Err(e) = conn.execute("COMMIT", []) {
        tracing::error!(error = %e, stage = "inserting", "commit failed");
        result.errors.push(format!("DB commit failed: {}", e));
        if let Err(rb) = conn.execute("ROLLBACK", []) {
            tracing::error!(error = %rb, stage = "inserting", "rollback after failed commit failed");
        }
    }

    // Force WAL checkpoint to consolidate writes and reduce post-scan CPU activity
    // TRUNCATE mode moves all data from WAL to main DB and truncates the WAL file
    if let Err(e) = conn.execute("PRAGMA wal_checkpoint(TRUNCATE)", []) {
        tracing::warn!(error = %e, stage = "inserting", "WAL checkpoint failed");
    }

    // Collect Phase-1 errors. EXTEND, don't assign: result.errors already
    // holds write-loop errors (in-place re-parse failures, COMMIT/ROLLBACK
    // failures) that an assignment would silently discard — that exact bug
    // hid the nested-transaction failure in the re-parse path for weeks.
    let phase1_errors = match Arc::try_unwrap(errors) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc) => arc.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    };
    result.errors.extend(phase1_errors);
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

    let span = tracing::info_span!("scan", root_id);
    let _g = span.enter();

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
        tracing::error!(root_id, error = %e, "failed to update scan timestamp");
    }

    // Persist scan errors so they survive app restarts.
    if let Err(e) = crate::db::update_scan_root_errors(&conn, root_id, &result.errors) {
        tracing::error!(root_id, error = %e, "failed to persist scan errors");
    }

    Ok(RegisteredScanOutcome { result, reconcile })
}

#[cfg(test)]
mod inplace_tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use rusqlite::params;
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    /// Re-touching a FITS file (simulating a restore round-trip or a user
    /// edit) must NOT remove the frame from its session. The scanner should
    /// re-parse and UPDATE in place, preserving frames.id so the
    /// session_members → frames → files JOIN stays intact.
    #[test]
    fn rescan_after_mtime_change_preserves_session_members() {
        let scan = TempDir::new().unwrap();
        let f = scan.path().join("M33/L_001.fits");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        crate::archive::restore::tests::write_minimal_fits(&f);

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames_set (id, name) VALUES (1, 'M33')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')",
            [],
        )
        .unwrap();

        // First scan inserts the file/frame; we then manually link it into a session.
        let cancel = Arc::new(AtomicBool::new(false));
        let scan1 = scan_directory_parallel(
            scan.path(),
            1,
            &conn,
            &NullEmitter,
            false,
            cancel.clone(),
            false,
        );
        assert!(scan1.errors.is_empty(), "first scan must succeed: {:?}", scan1.errors);
        assert!(!scan1.cancelled);

        let frame_id: i64 = conn
            .query_row(
                "SELECT f.id FROM frames f JOIN files fi ON fi.id = f.file_id WHERE fi.path = ?1",
                [f.to_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)",
            params![frame_id],
        )
        .unwrap();

        // Touch the file: write the same bytes back, which advances mtime
        // by at least one filesystem tick. Sleep first so the new mtime is
        // strictly greater than the old one (avoids same-second collision).
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let bytes = std::fs::read(&f).unwrap();
        std::fs::write(&f, bytes).unwrap();

        // Rescan via the serial path (the path Task 4 fixes).
        let scan2 = scan_directory(scan.path(), &conn, None, false, false, 1);
        assert!(scan2.errors.is_empty(), "rescan must succeed: {:?}", scan2.errors);
        assert!(!scan2.cancelled);

        // frame.id must be unchanged AND the session membership must survive.
        let frame_id_after: i64 = conn
            .query_row(
                "SELECT f.id FROM frames f JOIN files fi ON fi.id = f.file_id WHERE fi.path = ?1",
                [f.to_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            frame_id, frame_id_after,
            "frame.id must be preserved across in-place re-parse"
        );
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_members WHERE frame_id = ?1",
                params![frame_id_after],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "session membership must survive the re-parse");
    }

    /// Recovery path: a previous scan inserted the `files` row but the FITS
    /// parse failed, leaving zero `frames` rows. After the user fixes the
    /// file (mtime advances), the next scan must INSERT a fresh `frames` row
    /// rather than bailing.
    #[test]
    fn rescan_recovers_orphaned_files_row_with_no_frames() {
        let scan = TempDir::new().unwrap();
        let f = scan.path().join("M33/L_002.fits");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        crate::archive::restore::tests::write_minimal_fits(&f);

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()],
        )
        .unwrap();

        // Manually insert a `files` row with no matching `frames`. Simulates
        // the orphan that a previous parse-failure scan would have left.
        let f_size = std::fs::metadata(&f).unwrap().len() as i64;
        let f_mtime = chrono::DateTime::<chrono::Utc>::from(
            std::fs::metadata(&f).unwrap().modified().unwrap(),
        )
        .to_rfc3339();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (5000, ?1, 'L_002.fits', ?2, ?3, 'FITS')",
            params![f.to_str().unwrap(), f_size, f_mtime],
        )
        .unwrap();

        // Touch file so the scanner classifies it as "modified" (not
        // "unchanged"). Sleep first so the new mtime is strictly greater.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let bytes = std::fs::read(&f).unwrap();
        std::fs::write(&f, bytes).unwrap();

        // Rescan via the serial path that Task 4 fixed.
        let result = scan_directory(scan.path(), &conn, None, false, false, 1);
        assert!(result.errors.is_empty(), "rescan must succeed: {:?}", result.errors);
        assert!(!result.cancelled);

        // Now there must be exactly one frames row pointing at file_id=5000.
        let frame_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM frames WHERE file_id = 5000",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(frame_count, 1, "orphaned files row must get a fresh frames row");
    }

    /// Same invariant as `rescan_after_mtime_change_preserves_session_members`
    /// but exercises `scan_directory_parallel` — the path monitoring uses. Until
    /// Task 5 lands, this test fails because the parallel scan still does
    /// DELETE + re-INSERT in the classification phase.
    #[test]
    fn parallel_rescan_after_mtime_change_preserves_session_members() {
        let scan = TempDir::new().unwrap();
        let f = scan.path().join("M33/L_001.fits");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        crate::archive::restore::tests::write_minimal_fits(&f);

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M33')", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let scan1 = scan_directory_parallel(
            scan.path(), 1, &conn, &NullEmitter, false, cancel.clone(), false,
        );
        assert!(scan1.errors.is_empty(), "first scan must succeed: {:?}", scan1.errors);

        let frame_id: i64 = conn.query_row(
            "SELECT f.id FROM frames f JOIN files fi ON fi.id = f.file_id WHERE fi.path = ?1",
            [f.to_str().unwrap()], |r| r.get(0),
        ).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)",
            params![frame_id]).unwrap();

        // Touch — advance mtime by at least one filesystem tick.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let bytes = std::fs::read(&f).unwrap();
        std::fs::write(&f, bytes).unwrap();

        // Rescan via the parallel path.
        let cancel2 = Arc::new(AtomicBool::new(false));
        let scan2 = scan_directory_parallel(
            scan.path(), 1, &conn, &NullEmitter, false, cancel2, false,
        );
        assert!(scan2.errors.is_empty(), "rescan must succeed: {:?}", scan2.errors);
        assert!(!scan2.cancelled);
        // The re-parse must actually have run — not failed silently leaving
        // the old rows in place. files_processed counts only successful
        // re-parses, and modified_at only advances if the files UPDATE ran.
        assert_eq!(scan2.files_processed, 1, "the modified file must be re-parsed");
        let on_disk_mtime = chrono::DateTime::<chrono::Utc>::from(
            std::fs::metadata(&f).unwrap().modified().unwrap(),
        )
        .to_rfc3339();
        let db_mtime: String = conn.query_row(
            "SELECT modified_at FROM files WHERE path = ?1",
            [f.to_str().unwrap()], |r| r.get(0),
        ).unwrap();
        assert_eq!(db_mtime, on_disk_mtime,
            "files.modified_at must be refreshed by the in-place re-parse");

        let frame_id_after: i64 = conn.query_row(
            "SELECT f.id FROM frames f JOIN files fi ON fi.id = f.file_id WHERE fi.path = ?1",
            [f.to_str().unwrap()], |r| r.get(0),
        ).unwrap();
        assert_eq!(frame_id, frame_id_after,
            "frame.id must be preserved across parallel in-place re-parse");

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM session_members WHERE frame_id = ?1",
            params![frame_id_after], |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 1, "session membership must survive the parallel re-parse");
    }

    /// frames.override = 1 means "user has edited; scanner must not undo".
    /// A re-parse triggered by mtime drift must refresh the `files` row
    /// (so the file stops being classified as modified) but leave the
    /// user's frame edits and the override flag fully intact.
    #[test]
    fn parallel_rescan_preserves_user_override_edits() {
        let scan = TempDir::new().unwrap();
        let f = scan.path().join("M33/L_003.fits");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        crate::archive::restore::tests::write_minimal_fits(&f);

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()]).unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let scan1 = scan_directory_parallel(
            scan.path(), 1, &conn, &NullEmitter, false, cancel.clone(), false,
        );
        assert!(scan1.errors.is_empty(), "first scan must succeed: {:?}", scan1.errors);

        // User edits the frame's metadata (the FITS header says OBJECT = 'M33').
        conn.execute(
            "UPDATE frames SET object = 'NGC 598 (edited)', override = 1
             WHERE file_id = (SELECT id FROM files WHERE path = ?1)",
            [f.to_str().unwrap()],
        ).unwrap();

        // Touch — advance mtime by at least one filesystem tick.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let bytes = std::fs::read(&f).unwrap();
        std::fs::write(&f, bytes).unwrap();

        let cancel2 = Arc::new(AtomicBool::new(false));
        let scan2 = scan_directory_parallel(
            scan.path(), 1, &conn, &NullEmitter, false, cancel2, false,
        );
        assert!(scan2.errors.is_empty(), "rescan must succeed: {:?}", scan2.errors);
        assert_eq!(scan2.files_processed, 1, "the modified file must be re-parsed");

        let (object, override_flag, db_mtime): (String, i64, String) = conn.query_row(
            "SELECT fr.object, fr.override, fi.modified_at
             FROM frames fr JOIN files fi ON fi.id = fr.file_id
             WHERE fi.path = ?1",
            [f.to_str().unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(object, "NGC 598 (edited)",
            "user-edited OBJECT must survive the re-parse");
        assert_eq!(override_flag, 1, "override flag must survive the re-parse");

        // The files row must still be refreshed so the file is classified
        // as unchanged on the next scan (no endless re-parse loop).
        let on_disk_mtime = chrono::DateTime::<chrono::Utc>::from(
            std::fs::metadata(&f).unwrap().modified().unwrap(),
        )
        .to_rfc3339();
        assert_eq!(db_mtime, on_disk_mtime,
            "files.modified_at must be refreshed even when override = 1");
    }
}

/// Volume-aware move-detection guard + its two-site wiring. Uses the real
/// `mono.fits` fixture from the rustafits submodule (not a synthetic
/// minimal FITS) so `compute_header_fingerprint` runs against genuine
/// header content, per the project's real-data-first rule.
#[cfg(test)]
mod moved_file_guard_tests {
    use super::*;
    use crate::db::schema::init_db;
    use rusqlite::params;
    use tempfile::TempDir;

    const MONO_FIXTURE: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../rustafits/tests/mono.fits");

    /// Insert a fake `files` + `fits_header` row carrying the real
    /// fingerprint of the fixture, standing in for a "previously scanned"
    /// file at `path` (which is never actually written to disk).
    fn seed_fake_old_row(conn: &Connection, file_id: i64, path: &str, header_text: &str) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, 'old.fits', 100, '2025-01-01T00:00:00Z', 'FITS')",
            params![file_id, path],
        )
        .unwrap();
        insert_fits_header(conn, file_id, header_text).unwrap();
    }

    /// Site 1 (`process_file`): the old row's path lives under a scan root
    /// whose directory is missing (simulated unmounted volume). The guard
    /// must block the flip so the still-valid-but-offline original is left
    /// alone, and the real fixture at the new path is inserted as a new row.
    #[test]
    fn site1_guard_blocks_flip_when_old_scan_root_is_unavailable() {
        let tmp = TempDir::new().unwrap();

        // Offline root: registered in scan_roots but never created on disk.
        let offline_root = tmp.path().join("offline-volume/astro");
        // Online root: real, existing directory for the "new" file.
        let online_root = tmp.path().join("online-volume");
        std::fs::create_dir_all(&online_root).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [offline_root.to_str().unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (2, ?1)",
            [online_root.to_str().unwrap()],
        )
        .unwrap();

        let (_frame, header_text) =
            parse_fits_with_header(&PathBuf::from(MONO_FIXTURE), 0).unwrap();

        let old_path = offline_root.join("L_001.fits");
        let old_path_str = old_path.to_str().unwrap().to_string();
        seed_fake_old_row(&conn, 900, &old_path_str, &header_text);

        let new_path = online_root.join("L_001.fits");
        std::fs::copy(MONO_FIXTURE, &new_path).unwrap();

        let mut hash_errors = Vec::new();
        let result = process_file(&new_path, &conn, false, false, 2, &mut hash_errors).unwrap();
        assert!(
            result.is_some(),
            "guard must let the new file insert instead of silently treating it as a move"
        );

        let old_path_after: String = conn
            .query_row("SELECT path FROM files WHERE id = 900", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            old_path_after, old_path_str,
            "guard must block the flip: the offline original's path must stay put"
        );

        let total_files: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            total_files, 2,
            "both the offline original and the new file must exist — no orphaning"
        );
    }

    /// Control: same fingerprint match, but the old row's scan root
    /// directory DOES exist on disk (only the file itself is gone) — a
    /// genuine move. Current flip behavior must be preserved.
    #[test]
    fn site1_flip_still_works_when_old_scan_root_is_available() {
        let tmp = TempDir::new().unwrap();

        let old_root = tmp.path().join("root-a");
        let new_root = tmp.path().join("root-b");
        std::fs::create_dir_all(&old_root).unwrap();
        std::fs::create_dir_all(&new_root).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [old_root.to_str().unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (2, ?1)",
            [new_root.to_str().unwrap()],
        )
        .unwrap();

        let (_frame, header_text) =
            parse_fits_with_header(&PathBuf::from(MONO_FIXTURE), 0).unwrap();

        // Old row's path is under an existing root directory, but the file
        // itself was never written there (it "moved away").
        let old_path = old_root.join("L_001.fits");
        let old_path_str = old_path.to_str().unwrap().to_string();
        seed_fake_old_row(&conn, 901, &old_path_str, &header_text);

        let new_path = new_root.join("L_001.fits");
        std::fs::copy(MONO_FIXTURE, &new_path).unwrap();

        let mut hash_errors = Vec::new();
        let result = process_file(&new_path, &conn, false, false, 2, &mut hash_errors).unwrap();
        assert!(
            result.is_none(),
            "a genuine move must short-circuit with Ok(None), not insert a new row"
        );

        let path_after: String = conn
            .query_row("SELECT path FROM files WHERE id = 901", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            path_after,
            new_path.to_str().unwrap(),
            "flip must update files.path to the new location"
        );

        let total_files: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total_files, 1, "no duplicate row should be inserted for a genuine move");
    }

    // -- helper unit tests (also cover Site 2, which isn't independently
    // testable without standing up the full parallel scan pipeline; see
    // the report for that gap) --

    #[test]
    fn path_has_root_prefix_boundary_cases() {
        assert!(
            path_has_root_prefix("/a/b", "/a/b"),
            "the root path itself counts as under the root"
        );
        assert!(
            path_has_root_prefix("/a/b/x.fits", "/a/b"),
            "a child path is under the root"
        );
        assert!(
            !path_has_root_prefix("/a/bc/x.fits", "/a/b"),
            "a sibling directory sharing a string prefix must NOT match"
        );
        assert!(
            !path_has_root_prefix("/x/y/z.fits", "/a/b"),
            "an unrelated path must not match"
        );
    }

    /// A `scan_roots.path` value with a trailing separator (Windows drive
    /// roots, or a legacy/manually-edited row) must still match its own
    /// descendants — the naive `strip_prefix` would otherwise consume the
    /// separator as part of the literal prefix and never match.
    #[test]
    fn path_has_root_prefix_trailing_separator_cases() {
        assert!(
            path_has_root_prefix("/a/b/x.fits", "/a/b/"),
            "a root stored with a trailing slash must match its direct children"
        );
        assert!(
            path_has_root_prefix("/a/b/c/x.fits", "/a/b/"),
            "a root stored with a trailing slash must match nested descendants"
        );
        assert!(
            !path_has_root_prefix("/a/bc/x.fits", "/a/b/"),
            "a sibling directory sharing a string prefix must still NOT match"
        );
    }

    /// Degenerate cases, pinned deliberately rather than left as accidental
    /// fallout of the trailing-separator trim.
    #[test]
    fn path_has_root_prefix_degenerate_cases() {
        // Root "/" trims to "" — every absolute unix path is a descendant
        // of the filesystem root. This is semantically correct, not a bug.
        assert!(
            path_has_root_prefix("/anything/at/all.fits", "/"),
            "a root of '/' legitimately owns every absolute unix path"
        );

        // Windows drive root "D:\" trims to "D:".
        assert!(
            path_has_root_prefix("D:\\x.fits", "D:\\"),
            "a Windows drive root stored with its trailing backslash must match its children"
        );
    }

    #[test]
    fn path_under_unavailable_scan_root_resolution() {
        let tmp = TempDir::new().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let missing_root = tmp.path().join("missing");
        let existing_root = tmp.path().join("existing");
        std::fs::create_dir_all(&existing_root).unwrap();

        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [missing_root.to_str().unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (2, ?1)",
            [existing_root.to_str().unwrap()],
        )
        .unwrap();

        let under_missing = missing_root.join("a/b.fits");
        assert_eq!(
            path_under_unavailable_scan_root(&conn, under_missing.to_str().unwrap()).unwrap(),
            Some(missing_root.to_str().unwrap().to_string()),
            "a path under a missing scan root must report unavailable, naming that root"
        );

        let under_existing = existing_root.join("a/b.fits");
        assert_eq!(
            path_under_unavailable_scan_root(&conn, under_existing.to_str().unwrap()).unwrap(),
            None,
            "a path under an existing scan root must report available"
        );

        let elsewhere = tmp.path().join("elsewhere/c.fits");
        assert_eq!(
            path_under_unavailable_scan_root(&conn, elsewhere.to_str().unwrap()).unwrap(),
            None,
            "a path under no known scan root must behave as today (available)"
        );
    }

    /// Nested scan roots (a more specific root registered inside a broader
    /// one — e.g. removable media mounted under an always-present host
    /// path) must resolve ownership to the longest (most specific) match,
    /// not the first/shortest one.
    #[test]
    fn path_under_unavailable_scan_root_longest_match_wins() {
        let tmp = TempDir::new().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let outer_root = tmp.path().join("host-mount");
        std::fs::create_dir_all(&outer_root).unwrap();
        // Inner root is nested under the outer root but never created on
        // disk (e.g. removable media currently unmounted).
        let inner_root = outer_root.join("removable-drive");

        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [outer_root.to_str().unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (2, ?1)",
            [inner_root.to_str().unwrap()],
        )
        .unwrap();

        let p = inner_root.join("x.fits");
        assert_eq!(
            path_under_unavailable_scan_root(&conn, p.to_str().unwrap()).unwrap(),
            Some(inner_root.to_str().unwrap().to_string()),
            "the more specific (offline) inner root must win over the online outer root"
        );
    }
}
