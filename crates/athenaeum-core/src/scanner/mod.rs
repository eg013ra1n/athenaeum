// File scanner module
// Handles directory traversal and metadata extraction

use crate::db::{
    file_exists, insert_file, insert_frame, insert_fits_header,
    rebuild_duplicate_groups_cache, rebuild_folder_similarity_cache,
};
use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::fits_parser::{
    calibrated_light_identity, extract_xisf_header, parse_fits_with_header, parse_xisf,
    CalibratedIdentity,
};
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
pub(crate) fn path_to_utf8(path: &std::path::Path) -> anyhow::Result<String> {
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
pub(crate) fn path_has_root_prefix(path: &str, root: &str) -> bool {
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
    /// Set when the parsed header identifies a calibrated-LIGHT artifact
    /// (design §4). The Phase-2 write loop diverts these onto the DB-touching
    /// reconcile-adopt path instead of inserting a `files`/`frames` row —
    /// `process_file_parallel` has no `conn`, so the decision is carried here.
    pub calibrated_identity: Option<CalibratedIdentity>,
}

/// Progress event sent to frontend via Tauri events
#[derive(Clone, serde::Serialize, ts_rs::TS)]
pub struct ScanProgressEvent {
    pub current: usize,
    pub total: usize,
    pub current_file: Option<String>,
    pub percent: f64,
    pub root_id: i64,
    pub phase: String, // "discovery", "processing", "inserting", "calibrating"
}

/// Scan completion event
#[derive(Clone, serde::Serialize, ts_rs::TS)]
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
    /// Calibrated-LIGHT artifacts (design §4.3) whose header identity matches a
    /// tracked output whose file ALSO still exists — a duplicate copy the user
    /// should be told about. Never registered as frames; surfaced only here.
    pub calibrated_duplicates: Vec<CalibratedDuplicate>,
}

/// One duplicate calibrated-LIGHT sighting (design §4.3): the tracked output
/// that stays authoritative (`kept_path`) and the just-scanned copy
/// (`duplicate_path`). Carried to the frontend in the scan-finished
/// notification; the tracking row is never changed.
#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CalibratedDuplicate {
    pub kept_path: String,
    pub duplicate_path: String,
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
        calibrated_duplicates: Vec::new(),
        new_file_ids: Vec::new(),
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

    // Duplicate calibrated-LIGHT sightings collected during this scan (§4.3).
    let mut calibrated_duplicates: Vec<CalibratedDuplicate> = Vec::new();

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
                "SELECT id, size, modified_at,
                        EXISTS(SELECT 1 FROM frames WHERE file_id = files.id)
                 FROM files WHERE path = ?1",
                rusqlite::params![path_str],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, bool>(3)?,
                    ))
                },
            )
            .ok();
        if let Some((file_id, db_size, db_modified, has_frame)) = existing {
            let on_disk = std::fs::metadata(file_path).ok().map(|m| {
                let size = m.len() as i64;
                let modified = m
                    .modified()
                    .ok()
                    .map(|t| chrono::DateTime::<Utc>::from(t).to_rfc3339());
                (size, modified)
            });
            // A files row with no frames row is never "unchanged" — route it
            // through re-parse so the frame_count == 0 self-heal in
            // reparse_and_update_in_place can recreate the frame (audit C3).
            let unchanged = has_frame
                && matches!(
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
                file_path, file_id, conn, use_content_hash, unique_camera, root_id, false,
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
            &mut calibrated_duplicates,
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
    result.calibrated_duplicates = calibrated_duplicates;

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
    calibrated_duplicates_out: &mut Vec<CalibratedDuplicate>,
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

    // Calibrated-LIGHT artifacts (design §4) are recognized by their header
    // cards and never registered as frames — they take the reconcile-adopt
    // path instead. Runs BEFORE the moved-file / new-file insert logic below.
    if let Some(ref header) = header_text {
        let keys = parse_stored_header_keys(format.clone(), header);
        if let Some(identity) = calibrated_light_identity(&keys) {
            // A received project contribution (slice 4) carries an ATH_PRJ stamp
            // ON TOP of the calibrated-light cards — route it to the sibling
            // project-contribution reconcile instead. Checked FIRST: the file
            // also satisfies `calibrated_light_identity`.
            if identity.project_id.is_some() {
                reconcile_project_contribution(conn, path, &current_path, &identity, root_id)?;
                return Ok(None);
            }
            reconcile_calibrated_light(
                conn,
                path,
                &current_path,
                &identity,
                root_id,
                calibrated_duplicates_out,
            )?;
            return Ok(None);
        }
    }

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
        uuid: None,
        updated_at: None,
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

/// Reconcile a calibrated-LIGHT artifact against the `light_calibrations`
/// tracking table (design §4). Never registers a `files`/`frames` row — the
/// file is an out-of-catalog artifact (§3). Four branches, keyed on whether an
/// existing tracking row matches this file's header identity:
///
/// 1. **Known path** — a row's `output_path` equals the scanned path → no-op.
/// 2. **Moved** — a row matches identity but its `output_path` no longer exists
///    on disk → repair `output_path` to the new location, `info!`.
/// 3. **Duplicate copy** — a row matches identity and its `output_path` ALSO
///    still exists → record the (kept, duplicate) pair, `warn!`; row untouched.
/// 4. **Unknown** — no row matches → adopt: resolve the source frame by uuid
///    then filename (disambiguated by the copied-through OBJECT/DATE-OBS —
///    filenames collide across nights); resolved to exactly one → INSERT a
///    tracking row (`info!`); zero or ambiguous → `warn!` and skip so a later
///    scan (once the source is cataloged / disambiguated) succeeds.
fn reconcile_calibrated_light(
    conn: &Connection,
    path: &Path,
    current_path: &str,
    identity: &CalibratedIdentity,
    root_id: i64,
    calibrated_duplicates_out: &mut Vec<CalibratedDuplicate>,
) -> anyhow::Result<()> {
    use crate::db::light_calibrations::{
        find_by_identity_disambiguated, update_output_path, upsert_light_calibration, FlatNormMode,
        LightCalRow, LIGHT_CAL_ENGINE_VERSION,
    };

    // Disambiguated identity match (§4): the filename fallback is narrowed by
    // the copied-through OBJECT/DATE-OBS so a colliding filename can't repair
    // (branch 2) or duplicate-flag (branch 3) against a row that belongs to a
    // different source frame.
    let existing = find_by_identity_disambiguated(
        conn,
        identity.source_uuid.as_deref(),
        identity.source_filename.as_deref(),
        identity.source_object.as_deref(),
        identity.source_date_obs.as_deref(),
    )?;

    if let Some(row) = existing {
        if row.output_path == current_path {
            // Branch 1: this is the tracked file at its known location.
            tracing::debug!(root_id, path = %current_path, "known calibrated light — skipping ingestion");
        } else if !Path::new(&row.output_path).exists() {
            // Branch 2: the tracked file is gone from its old location and this
            // is it under a new path — repair the pointer (file-relink philosophy).
            update_output_path(conn, row.id, current_path)?;
            tracing::info!(
                root_id,
                old_path = %row.output_path,
                path = %current_path,
                "calibrated light moved — output_path repaired"
            );
        } else if std::fs::canonicalize(&row.output_path)
            .ok()
            .zip(std::fs::canonicalize(Path::new(current_path)).ok())
            .is_some_and(|(a, b)| a == b)
        {
            // Branch 2b: both spellings resolve to one physical file (Windows
            // trailing-dot/case normalization, symlinked mounts). Repair the
            // pointer to the scanned spelling — flagging it as a duplicate would
            // warn on every scan forever.
            update_output_path(conn, row.id, current_path)?;
            tracing::info!(
                root_id,
                old_path = %row.output_path,
                path = %current_path,
                "calibrated light path spelling normalized — output_path repaired"
            );
        } else {
            // Branch 3: the tracked file still exists elsewhere, so this is a
            // duplicate copy. Signal it; leave the tracking row untouched.
            tracing::warn!(
                root_id,
                kept_path = %row.output_path,
                duplicate_path = %current_path,
                "duplicate calibrated light detected"
            );
            calibrated_duplicates_out.push(CalibratedDuplicate {
                kept_path: row.output_path.clone(),
                duplicate_path: current_path.to_string(),
            });
        }
        return Ok(());
    }

    // Branch 4: no tracking row — adopt if the source frame is cataloged.
    let frame_id = match resolve_calibrated_source_frame(conn, identity)? {
        SourceResolution::Resolved(frame_id) => frame_id,
        SourceResolution::NotFound => {
            tracing::warn!(
                root_id,
                path = %current_path,
                source_uuid = identity.source_uuid.as_deref().unwrap_or(""),
                source_filename = identity.source_filename.as_deref().unwrap_or(""),
                "calibrated light source not in catalog — deferring adoption"
            );
            return Ok(());
        }
        SourceResolution::Ambiguous(count) => {
            // Two+ catalog frames share this filename and the copied-through
            // OBJECT/DATE-OBS didn't narrow it to one. Binding arbitrarily would
            // pick the wrong frame and (via the ON CONFLICT(frame_id) upsert)
            // collapse multiple calibrated files onto one row — so defer. A
            // later scan adopts idempotently once the catalog disambiguates.
            tracing::warn!(
                count,
                filename = identity.source_filename.as_deref().unwrap_or(""),
                path = %current_path,
                "ambiguous calibrated-light source, adoption deferred"
            );
            return Ok(());
        }
    };

    // Resolve master references (uuid then path) to calibration-set ids where
    // possible; an unresolvable reference records NULL (honest — a rebuilt
    // catalog may no longer carry that master).
    let dark_set_id = identity.dark.as_ref().and_then(|m| resolve_master_set_id(conn, m));
    let flat_set_id = identity.flat.as_ref().and_then(|m| resolve_master_set_id(conn, m));
    let bias_set_id = identity.bias.as_ref().and_then(|m| resolve_master_set_id(conn, m));

    // A flat was normalized iff a flat was applied AND the recorded divisor is
    // not the 1.0 sentinel (§2: ATH_CFNM = 1.0 means normalization was off).
    let flat_norm_applied = identity.flat.is_some()
        && identity
            .flat_norm_divisor
            .map(|d| (d - 1.0).abs() > 1e-9)
            .unwrap_or(false);

    // Best-effort content hash of the artifact on disk (same function the
    // engine uses); a failure records an empty hash rather than aborting.
    let output_hash = crate::duplicates::compute_xxhash(path).unwrap_or_default();

    let row = LightCalRow {
        id: 0,
        frame_id: Some(frame_id),
        source_uuid: identity.source_uuid.clone(),
        source_filename: identity.source_filename.clone(),
        output_path: current_path.to_string(),
        dark_set_id,
        flat_set_id,
        bias_set_id,
        calstat: identity.calstat.clone(),
        flat_norm_applied,
        // The calibrated file's header carries the normalization DIVISOR
        // (`ATH_CFNM`) but not the STATISTIC used, so an adopted row can't tell
        // central-third from trimmed — default to central-third. Harmless for
        // staleness: `derive_status` only compares the mode when both the row
        // and the wanted run normalize, and a mode-only mismatch just prompts a
        // (correct) re-calibrate.
        flat_norm_mode: FlatNormMode::CentralThird.as_wire_str().to_string(),
        // The advanced parameters aren't fully recoverable from the file:
        // `bias_fallback` was never stamped (it's a build-time decision, not a
        // card), so a faithful reconstruction is impossible. Default to `'{}'`,
        // which normalizes to `LightCalParams::default()` in `derive_status` —
        // same harmless-for-staleness tradeoff as `flat_norm_mode` above; a real
        // divergence just prompts a (correct) re-calibrate.
        cal_params: "{}".to_string(),
        // Recoverable, unlike the two above: per-channel scaling stamps
        // `ATH_CCFA` on the output, so an adopted file can say what it got. An
        // absent card is "not stated" (a globally-normalized output, or one from
        // before the feature) and stays NULL — which `derive_status` reads as
        // global, the honest answer for both.
        cfa_scaling_applied: identity.cfa_scaling_applied,
        output_hash,
        // Carry the file's own engine version so staleness derivation (§5)
        // correctly flags a file built by an older engine.
        engine_version: identity.engine_version.unwrap_or(LIGHT_CAL_ENGINE_VERSION),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    upsert_light_calibration(conn, &row)?;
    tracing::info!(root_id, frame_id, path = %current_path, "adopted calibrated light");
    Ok(())
}

/// Reconcile a received project-contribution artifact (Stage-II collaboration,
/// slice 4, spec §7) against the `project_contributions` tracking table. Runs
/// INSTEAD of [`reconcile_calibrated_light`] when the file carries an `ATH_PRJ`
/// project stamp (a published contribution is a calibrated light, so it also
/// carries `ATH_CSRC` — the project card is checked first). Like a calibrated
/// light it is an out-of-catalog artifact: it NEVER registers a `files`/`frames`
/// row on any branch. Four branches, keyed on the contribution table:
///
/// 1. **Known** — a row's `landed_path` equals the scanned path → no-op.
/// 2. **Moved** — a row matches `(project_id, xxh3-of-file)` and its
///    `landed_path` no longer exists on disk → repair `landed_path`, `info!`.
/// 3. **Duplicate** — a row matches `(project_id, xxh3)` and its `landed_path`
///    ALSO still exists → `warn!`, row untouched (a second copy of the bytes).
/// 4. **Unknown** — no row matches → `warn!` and defer. NEVER auto-insert: a
///    contribution row requires hub-anchored package context we don't have at
///    scan time (re-download via the project page). Idempotent on re-scan.
fn reconcile_project_contribution(
    conn: &Connection,
    path: &Path,
    current_path: &str,
    identity: &CalibratedIdentity,
    root_id: i64,
) -> anyhow::Result<()> {
    use crate::db::collab_exchange::{
        find_contribution_by_landed_path, find_contribution_by_project_and_hash,
        update_contribution_landed_path,
    };

    // Guaranteed Some by the caller's `identity.project_id.is_some()` route, but
    // stay defensive: an empty id can't scope a lookup, so leave the file be.
    let Some(project_id) = identity.project_id.as_deref() else {
        return Ok(());
    };

    // Branch 1 (known): a row already tracks this exact path. Cheaper than the
    // content hash and must run first — a hash lookup would also match the
    // known file and mis-classify it as a duplicate.
    if find_contribution_by_landed_path(conn, current_path)?.is_some() {
        tracing::debug!(
            root_id,
            path = %current_path,
            project_id,
            "known project contribution — skipping ingestion"
        );
        return Ok(());
    }

    // Full-content hash (the manifest-anchored key — NOT the sampling hash).
    // A read failure means we can't classify moved-vs-duplicate-vs-unknown, so
    // defer rather than risk a wrong branch; the file still never enters the
    // catalog. Idempotent once the read succeeds on a later scan.
    let xxh3 = match crate::package::xxh3_full_file(path) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(
                root_id,
                path = %current_path,
                project_id,
                error = %e,
                "project contribution hash failed — leaving untouched"
            );
            return Ok(());
        }
    };

    match find_contribution_by_project_and_hash(conn, project_id, &xxh3)? {
        Some(row) => {
            if !Path::new(&row.landed_path).exists() {
                // Branch 2 (moved): the tracked copy is gone from its old
                // location and this is it under a new path — repair the pointer.
                update_contribution_landed_path(conn, row.id, current_path)?;
                tracing::info!(
                    root_id,
                    old_path = %row.landed_path,
                    path = %current_path,
                    project_id,
                    "project contribution moved — landed_path repaired"
                );
            } else {
                // Branch 3 (duplicate): the tracked copy still exists elsewhere,
                // so this is a second copy. Leave the tracking row untouched.
                tracing::warn!(
                    root_id,
                    kept = %row.landed_path,
                    duplicate = %current_path,
                    project_id,
                    "duplicate project contribution copy"
                );
            }
        }
        None => {
            // Branch 4 (unknown): no tracking row. NEVER auto-insert — a
            // contribution row needs hub package context we don't have here.
            tracing::warn!(
                path = %current_path,
                project_id,
                "project contribution without a tracking row — leaving untouched (re-download via the project page)"
            );
        }
    }
    Ok(())
}

/// Outcome of resolving the source LIGHT frame for an adopted calibrated
/// artifact. `Ambiguous` carries the candidate count so the caller can log it
/// and defer (idempotent retry once the catalog disambiguates), rather than
/// binding to an arbitrary frame.
enum SourceResolution {
    Resolved(i64),
    NotFound,
    Ambiguous(usize),
}

/// Resolve the source LIGHT frame for an adopted calibrated artifact:
///
/// 1. by `frames.uuid` first (unique key; DB-generated, may not survive a
///    catalog rebuild), then
/// 2. by `files.filename` (indexed), disambiguated with the OBJECT + DATE-OBS
///    the calibrated file copied through (design §7).
///
/// The bare-filename fallback is nondeterministic on its own: astro filenames
/// collide across nights/objects (`L_0001.fits`), so a `LIMIT 1` would bind the
/// wrong frame and, via the `ON CONFLICT(frame_id)` upsert, collapse several
/// calibrated files onto one row. So candidates are filtered by OBJECT and
/// DATE-OBS whenever the calibrated header carries them, and the result is
/// exactly-one → `Resolved`, zero → `NotFound`, more-than-one → `Ambiguous`
/// (never bound arbitrarily).
fn resolve_calibrated_source_frame(
    conn: &Connection,
    identity: &CalibratedIdentity,
) -> anyhow::Result<SourceResolution> {
    if let Some(uuid) = identity.source_uuid.as_deref() {
        let id: Option<i64> = conn
            .query_row(
                "SELECT id FROM frames WHERE uuid = ?1 LIMIT 1",
                rusqlite::params![uuid],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = id {
            return Ok(SourceResolution::Resolved(id));
        }
    }

    let Some(filename) = identity.source_filename.as_deref() else {
        return Ok(SourceResolution::NotFound);
    };

    // Every catalog frame sharing this filename, with the columns we
    // disambiguate on. Filtering happens in Rust so DATE-OBS can be compared
    // instant-aware (see `date_obs_matches`).
    let mut stmt = conn.prepare(
        "SELECT fr.id, fr.object, fr.date_obs
         FROM frames fr JOIN files fi ON fi.id = fr.file_id
         WHERE fi.filename = ?1",
    )?;
    let candidates: Vec<(i64, Option<String>, Option<String>)> = stmt
        .query_map(rusqlite::params![filename], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // OBJECT/DATE-OBS predicates shared with the tracking-row identity match
    // (`db::light_calibrations`) so both filename-fallback paths stay symmetric.
    use crate::db::light_calibrations::{date_obs_matches, object_matches};
    let matched: Vec<i64> = candidates
        .into_iter()
        .filter(|(_, object, date_obs)| {
            object_matches(identity.source_object.as_deref(), object.as_deref())
                && date_obs_matches(identity.source_date_obs.as_deref(), date_obs.as_deref())
        })
        .map(|(id, _, _)| id)
        .collect();

    Ok(match matched.as_slice() {
        [] => SourceResolution::NotFound,
        [id] => SourceResolution::Resolved(*id),
        many => SourceResolution::Ambiguous(many.len()),
    })
}

/// Resolve a master reference (`"<uuid> <path>"`) to its calibration-set id —
/// by the master frame's `uuid` first, then by its file `path`. Any miss
/// yields `None` (recorded as a NULL set id on the adopted row).
fn resolve_master_set_id(conn: &Connection, master: &crate::fits_parser::MasterRef) -> Option<i64> {
    if !master.uuid.is_empty() {
        let by_uuid: Option<i64> = conn
            .query_row(
                "SELECT csf.set_id FROM frames fr
                 JOIN calibration_set_frames csf ON csf.frame_id = fr.id
                 WHERE fr.uuid = ?1 LIMIT 1",
                rusqlite::params![master.uuid],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        if by_uuid.is_some() {
            return by_uuid;
        }
    }
    if !master.path.is_empty() {
        return conn
            .query_row(
                "SELECT csf.set_id FROM files fi
                 JOIN frames fr ON fr.file_id = fi.id
                 JOIN calibration_set_frames csf ON csf.frame_id = fr.id
                 WHERE fi.path = ?1 LIMIT 1",
                rusqlite::params![master.path],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
    }
    None
}

/// Re-parse a file whose on-disk metadata has drifted from the catalog and
/// UPDATE the existing files / frames / fits_header rows in place. Preserves
/// files.id and frames.id so every junction-table linkage (session_members,
/// calibration_set_frames, calibration_set_to_frames, frame_tags) survives.
///
/// Returns Ok(()) on success. On parse failure, leaves the DB row untouched
/// and returns Err — the caller should log it as a non-fatal error so the
/// rest of the scan continues.
///
/// `warn_override_skip` is for callers re-reading a file **Athenaeum itself
/// rewrote** (`resync_catalog_rows_from_disk`), where hitting the
/// `frames.override` skip is a notable event rather than the routine outcome
/// it is on an ordinary scan — see that function's docs.
#[allow(clippy::too_many_arguments)]
fn reparse_and_update_in_place(
    path: &Path,
    file_id: i64,
    conn: &Connection,
    use_content_hash: bool,
    unique_camera: bool,
    root_id: i64,
    warn_override_skip: bool,
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
            // A failed header extraction is the ONE way this function can
            // leave the stored header blob describing different bytes than the
            // frames columns it is about to write (light-cal copy-through
            // reads that blob). Keeping the previous snapshot beats deleting
            // it — per-field revert still works — but it must not be silent.
            let h = match extract_xisf_header(path) {
                Ok(h) => Some(h),
                Err(e) => {
                    tracing::warn!(
                        file_id,
                        path = %path.display(),
                        error = %e,
                        "xisf header extraction failed on re-parse — stored header keeps its previous contents"
                    );
                    None
                }
            };
            (f, h)
        }
    };

    let metadata_hash = compute_metadata_hash(size, &modified_dt, &filename);
    // KNOWN TRADE-OFF (smoke №8 review, deferred): this ~1.5MB sampling read runs
    // inside the caller's phase-2 batch WRITE transaction, so a re-scan with many
    // changed files extends the exclusive-lock hold and can SQLITE_BUSY concurrent
    // writers (the content-hash backfill skips-and-retries next launch; UI writes
    // ride the 5s busy_timeout). Hoisting the hash out of the tx needs a scanner
    // phase restructure — deliberately deferred; changed-file batches are small in
    // normal use (mtime/size drift only).
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
    if user_override && warn_override_skip {
        // The file was rewritten by Athenaeum (master rebuild), so its header
        // and the frames row now disagree BY DESIGN: user edits outrank header
        // values, but the stored header snapshot below still gets the new
        // bytes. Reachable in practice — `bulk_update_calibration_metadata`
        // sets override = 1 on every member of an edited calibration set, and
        // a master is the sole member of its own set. Not an error, but the
        // one place the catalog deliberately stops tracking the file.
        tracing::warn!(
            file_id,
            path = %path.display(),
            "rebuild resync skipped frames columns — user override"
        );
    }

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

/// Refresh the catalog rows of a file **Athenaeum itself just rewrote** from
/// what is now on disk: `files` (size/mtime/format/hashes), `frames` (every
/// header-derived column) and the `fits_header` snapshot, all in place, all in
/// one savepoint, preserving `files.id` and `frames.id` — and therefore every
/// junction-table linkage (calibration_set_frames, calibration_set_to_frames,
/// session_members, frame_tags).
///
/// Same writer the scanner's drift re-parse uses, so a rewritten file lands
/// exactly the rows a fresh ingest would have produced. The master-rebuild
/// path (`api::masters`) is the caller: a rebuild replaces the master file's
/// pixels AND its header cards, so without this the catalog would keep
/// describing the previous build's header.
///
/// Fixed argument choices, matching what `register_master` writes for a
/// freshly built master:
/// - `use_content_hash = false` — registration never stores one, and the
///   rewrite invalidates any value a hashing scan had filled in. The re-parse
///   writes NULL, which is honest and recoverable (the content-hash backfill
///   fills NULLs); a retained stale hash would not be.
/// - `unique_camera = false` / `root_id = 0` — `register_master` stores
///   INSTRUME exactly as parsed, with no " N<root_id>" suffix, and later
///   library scans classify the file as unchanged so they never add one.
///   Passing the suffix here would make a rebuilt master's INSTRUME differ
///   from a newly built one's.
///
/// Inherits the re-parse's `frames.override` rule: if the user has edited this
/// frame's metadata, the frames row is left alone and only files + the stored
/// header snapshot are refreshed. That is the intended precedence — user edits
/// outrank header values, and the snapshot must still describe the bytes now
/// on disk — but on THIS path it inverts the usual drift: the blob follows the
/// rewritten file while the frames columns deliberately do not. It is
/// reachable (`bulk_update_calibration_metadata` sets override = 1 on every
/// member of an edited calibration set, and a master is the sole member of its
/// own set), so the skip warns rather than passing silently; ordinary scans,
/// where the skip is routine, stay quiet.
pub fn resync_catalog_rows_from_disk(
    conn: &Connection,
    file_id: i64,
    path: &Path,
) -> anyhow::Result<()> {
    reparse_and_update_in_place(path, file_id, conn, false, false, 0, true)
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
        // is_master/override are written as i64 booleans, and the
        // migration-added columns (bayerpat, xbayroff, ybayroff, roworder —
        // see schema.rs) are included.
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
                override = ?29, swcreate = ?30, bayerpat = ?31, rotation = ?32,
                xbayroff = ?33, ybayroff = ?34, roworder = ?35
             WHERE file_id = ?36",
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
                frame.xbayroff,
                frame.ybayroff,
                frame.roworder,
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
    // Duplicate calibrated-LIGHT sightings (design §4.3); never registered.
    pub calibrated_duplicates: Vec<CalibratedDuplicate>,
    // File ids of rows freshly INSERTED by this scan (not re-parses/moves). Drives
    // the personal-sync auto-mode enqueue (task M2). Populated by the parallel
    // scan path only; empty on the non-parallel `scan_directory` path.
    pub new_file_ids: Vec<i64>,
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
        calibrated_duplicates: result.calibrated_duplicates.clone(),
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
        uuid: None,
        updated_at: None,
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

    // A calibrated-LIGHT artifact is recognized by its header cards, not its
    // path or IMAGETYP. Parse the (already-read) header and flag it so the
    // sequential write phase can reconcile-adopt it with DB access.
    let calibrated_identity = header.as_ref().and_then(|h| {
        let keys = parse_stored_header_keys(format, h);
        calibrated_light_identity(&keys)
    });

    Ok(FileProcessResult {
        file,
        frame,
        header,
        imagetyp,
        hash_error,
        calibrated_identity,
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
        calibrated_duplicates: Vec::new(),
        new_file_ids: Vec::new(),
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
    // modified_at, has_frame). We use this to classify each on-disk file as
    // new / unchanged / modified so that in-place modifications get
    // re-parsed instead of silently skipped (the previous path-only filter).
    // modified_at is stored as RFC3339 in the DB and compared as a string
    // for an exact-match check. The `id` is carried through so the write
    // loop below can dispatch UPDATE-in-place without a per-file
    // `SELECT id FROM files` (N+1) query. `has_frame` is the C3 guard: a
    // files row with no frames row is never "unchanged" (see below).
    tracing::debug!(root_id, "building existing files map from DB");
    let existing_files: std::collections::HashMap<String, (i64, i64, String, bool)> = {
        let mut map = std::collections::HashMap::new();
        match conn.prepare(
            "SELECT f.path, f.id, f.size, f.modified_at,
                    EXISTS(SELECT 1 FROM frames fr WHERE fr.file_id = f.id)
             FROM files f",
        ) {
            Ok(mut stmt) => {
                match stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, bool>(4)?,
                    ))
                }) {
                    Ok(rows) => {
                        for entry in rows.flatten() {
                            map.insert(entry.0, (entry.1, entry.2, entry.3, entry.4));
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
                Some((db_id, db_size, db_modified, has_frame)) => {
                    match std::fs::metadata(p) {
                        Ok(meta) => {
                            let on_disk_size = meta.len() as i64;
                            let on_disk_modified = meta
                                .modified()
                                .ok()
                                .map(|t| chrono::DateTime::<Utc>::from(t).to_rfc3339());
                            // A files row with no frames row is never
                            // "unchanged" — route it through re-parse so the
                            // frame_count == 0 self-heal in
                            // reparse_and_update_in_place can recreate the
                            // frame (audit C3).
                            let unchanged = on_disk_size == *db_size
                                && on_disk_modified.as_deref() == Some(db_modified.as_str())
                                && *has_frame;
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
    // Duplicate calibrated-LIGHT sightings collected during this scan (§4.3).
    let mut calibrated_duplicates: Vec<CalibratedDuplicate> = Vec::new();
    // File ids freshly inserted by this scan — drives personal-sync auto mode.
    let mut new_file_ids: Vec<i64> = Vec::new();

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

        // Calibrated-LIGHT artifacts (design §4) take the reconcile-adopt path
        // instead of frame registration. Detected during the parallel parse
        // phase (`calibrated_identity`); the DB-touching decision runs here,
        // inside the batch transaction, BEFORE any move/insert logic.
        if let Some(ref identity) = file_result.calibrated_identity {
            // A received project contribution (slice 4) carries an ATH_PRJ stamp
            // on top of the calibrated-light cards — divert to the sibling
            // project-contribution reconcile. Checked FIRST (it also satisfies
            // `calibrated_light_identity`); like a calibrated light it never
            // registers a `files`/`frames` row.
            if identity.project_id.is_some() {
                if let Err(e) = reconcile_project_contribution(
                    conn,
                    Path::new(&file_result.file.path),
                    &file_result.file.path,
                    identity,
                    root_id,
                ) {
                    tracing::error!(
                        root_id,
                        path = %file_result.file.path,
                        error = %e,
                        "project-contribution reconcile failed"
                    );
                    result.errors.push(format!(
                        "{}: project-contribution reconcile failed: {}",
                        file_result.file.path, e
                    ));
                }
                continue;
            }
            if let Err(e) = reconcile_calibrated_light(
                conn,
                Path::new(&file_result.file.path),
                &file_result.file.path,
                identity,
                root_id,
                &mut calibrated_duplicates,
            ) {
                tracing::error!(
                    root_id,
                    path = %file_result.file.path,
                    error = %e,
                    "calibrated-light reconcile failed"
                );
                result.errors.push(format!(
                    "{}: calibrated-light reconcile failed: {}",
                    file_result.file.path, e
                ));
            }
            continue;
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
                &path_buf, file_id, conn, use_content_hash, unique_camera, root_id, false,
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

        // New file path — atomic per file: files + frames + fits_header
        // either all commit or none do (SAVEPOINT nests inside the batch
        // transaction). Previously an insert_frame/insert_fits_header
        // failure after a successful insert_file committed a frameless or
        // headerless files row that every later scan classified as
        // "unchanged" and never repaired.
        let sp = match crate::db::SavepointGuard::new(conn, "scan_new_file") {
            Ok(sp) => sp,
            Err(e) => {
                errors.lock().unwrap().push(format!(
                    "{}: failed to open savepoint: {}",
                    file_result.file.path, e
                ));
                continue;
            }
        };
        let file_id = match insert_file(conn, &file_result.file) {
            Ok(id) => id,
            Err(e) => {
                errors.lock().unwrap().push(format!(
                    "{}: Failed to insert file: {}",
                    file_result.file.path, e
                ));
                continue; // sp drops → rollback
            }
        };
        // Update frame file_id in place (avoids clone of 32-field struct)
        file_result.frame.file_id = file_id;

        // Apply unique camera suffix to INSTRUME if enabled
        if unique_camera {
            if let Some(ref instrume) = file_result.frame.instrume {
                file_result.frame.instrume = Some(format!("{} N{}", instrume, root_id));
            }
        }

        let frame_id = match insert_frame(conn, &file_result.frame) {
            Ok(id) => id,
            Err(e) => {
                errors.lock().unwrap().push(format!(
                    "{}: Failed to insert frame: {}",
                    file_result.file.path, e
                ));
                continue; // sp drops → the files row rolls back too
            }
        };

        // Insert header if available — a failure here rolls the whole file
        // back so the next scan retries it, instead of committing a frame
        // whose header blob (metadata revert, light-cal copy-through) is
        // silently missing.
        if let Some(ref header) = file_result.header {
            if let Err(e) = insert_fits_header(conn, file_id, header) {
                tracing::error!(
                    file_id,
                    path = %file_result.file.path,
                    error = %e,
                    "failed to insert fits header; rolling file back"
                );
                errors.lock().unwrap().push(format!(
                    "{}: Failed to insert header: {}",
                    file_result.file.path, e
                ));
                continue; // sp drops → rollback
            }
        }

        if let Err(e) = sp.commit() {
            errors.lock().unwrap().push(format!(
                "{}: failed to commit file savepoint: {}",
                file_result.file.path, e
            ));
            continue;
        }

        result.files_processed += 1;
        // A genuinely new file+frame row — eligible for auto-mode sync.
        new_file_ids.push(file_id);

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
    if let Err(e) = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(())) {
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
    result.calibrated_duplicates = calibrated_duplicates;
    result.new_file_ids = new_file_ids;

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

/// Scan-time content hashing is opt-in via `duplicates.use_content_hash`
/// (default false).
///
/// `5aa58fed` briefly hard-coded this on so the transfer dedup handshake would
/// see the whole scanned library. That put a 3 x 512 KB sampling read on every
/// new/changed file — 27.86 GiB instead of 0.30 GiB on an 18 946-file library,
/// measured x5.9 cold. The dedup index is now filled by the explicit,
/// cancellable content-index job (`api::content_index`) instead, so the scan
/// hot path stays header-only.
pub(crate) fn scan_hashing_enabled(
    settings: &crate::settings::SettingsManager,
    conn: &rusqlite::Connection,
) -> bool {
    // A read failure must not pass silently just because the fallback happens
    // to equal the default: the visible symptom would be a user turning the
    // setting ON and scans quietly continuing not to hash.
    settings
        .get_duplicates_use_content_hash(conn)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "duplicates.use_content_hash read failed; scan hashing disabled");
            false
        })
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

    let use_content_hash = scan_hashing_enabled(&ctx.settings, &conn);
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

    /// Write a one-block FITS header with the shared minimal cards plus the
    /// caller's extra 80-column cards (OSC/CFA hardening Task 1).
    fn write_fits_with_extra_cards(path: &Path, extra: &[&str]) {
        fn card(s: &str) -> [u8; 80] {
            assert!(s.len() <= 80, "card too long: {:?} ({} bytes)", s, s.len());
            let mut out = [b' '; 80];
            out[..s.len()].copy_from_slice(s.as_bytes());
            out
        }

        let mut header: Vec<u8> = Vec::with_capacity(2880);
        for c in [
            "SIMPLE  =                    T / file conforms to FITS standard",
            "BITPIX  =                    8 / number of bits per data pixel",
            "NAXIS   =                    0 / number of data axes",
            "OBJECT  = 'M33     '           / target name",
            "TELESCOP= 'T       '",
            "INSTRUME= 'C       '",
            "IMAGETYP= 'Light   '",
        ] {
            header.extend_from_slice(&card(c));
        }
        for c in extra {
            header.extend_from_slice(&card(c));
        }
        header.extend_from_slice(&card("END"));
        while header.len() < 2880 {
            header.push(b' ');
        }
        std::fs::write(path, header).unwrap();
    }

    /// An in-place re-parse must carry the CFA phase offsets and row order into
    /// the existing frames row. A user who re-shot the header (driver upgrade,
    /// hand-edit) gets the new values; before this the columns would keep
    /// whatever the first ingest wrote.
    #[test]
    fn rescan_updates_bayer_offsets_and_roworder_in_place() {
        let scan = TempDir::new().unwrap();
        let f = scan.path().join("OSC/L_001.fits");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        // First ingest: an OSC frame whose header declares no offsets.
        write_fits_with_extra_cards(&f, &["BAYERPAT= 'RGGB    '"]);

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()],
        )
        .unwrap();

        let scan1 = scan_directory(scan.path(), &conn, None, false, false, 1);
        assert!(scan1.errors.is_empty(), "first scan must succeed: {:?}", scan1.errors);

        let (frame_id, xbayroff, ybayroff, roworder): (i64, Option<i64>, Option<i64>, Option<String>) =
            conn.query_row(
                "SELECT f.id, f.xbayroff, f.ybayroff, f.roworder
                 FROM frames f JOIN files fi ON fi.id = f.file_id WHERE fi.path = ?1",
                [f.to_str().unwrap()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(xbayroff, None, "absent XBAYROFF must not become 0 on ingest");
        assert_eq!(ybayroff, None, "absent YBAYROFF must not become 0 on ingest");
        assert_eq!(roworder, None);

        // Re-write the same file with the offsets present; mtime advances, so
        // the scanner re-parses and UPDATEs the existing rows in place.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write_fits_with_extra_cards(
            &f,
            &[
                "BAYERPAT= 'RGGB    '",
                "XBAYROFF=                    1",
                "YBAYROFF=                    0",
                "ROWORDER= 'TOP-DOWN'",
            ],
        );

        let scan2 = scan_directory(scan.path(), &conn, None, false, false, 1);
        assert!(scan2.errors.is_empty(), "rescan must succeed: {:?}", scan2.errors);
        assert_eq!(scan2.files_processed, 1, "the modified file must be re-parsed");

        let (frame_id_after, xbayroff, ybayroff, roworder): (
            i64,
            Option<i64>,
            Option<i64>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT f.id, f.xbayroff, f.ybayroff, f.roworder
                 FROM frames f JOIN files fi ON fi.id = f.file_id WHERE fi.path = ?1",
                [f.to_str().unwrap()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(frame_id, frame_id_after, "in-place re-parse must preserve frames.id");
        assert_eq!(xbayroff, Some(1));
        assert_eq!(ybayroff, Some(0));
        assert_eq!(roworder.as_deref(), Some("TOP-DOWN"));
    }

    /// The stored header blob is what light-calibration copy-through reads for
    /// the CFA cards, so it must never be left describing the PREVIOUS bytes
    /// while the frames columns describe the new ones — a stale non-blank blob
    /// card wins over the column fallback. Pins both halves of the documented
    /// re-parse contract ("files/frames/fits_header in one transaction") on a
    /// file whose BAYERPAT actually changed on disk.
    #[test]
    fn rescan_syncs_stored_header_blob_with_frames_columns() {
        let scan = TempDir::new().unwrap();
        let f = scan.path().join("OSC/L_001.fits");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        write_fits_with_extra_cards(&f, &["BAYERPAT= 'RGGB    '"]);

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()],
        )
        .unwrap();

        let scan1 = scan_directory(scan.path(), &conn, None, false, false, 1);
        assert!(scan1.errors.is_empty(), "first scan must succeed: {:?}", scan1.errors);

        let stored_header = |conn: &Connection| -> String {
            conn.query_row(
                "SELECT h.header FROM fits_header h
                 JOIN files fi ON fi.id = h.file_id WHERE fi.path = ?1",
                [f.to_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(stored_header(&conn).contains("BAYERPAT= 'RGGB"));

        // The camera driver now reports a different CFA phase — same file,
        // new header bytes, advanced mtime.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write_fits_with_extra_cards(&f, &["BAYERPAT= 'GRBG    '"]);

        let scan2 = scan_directory(scan.path(), &conn, None, false, false, 1);
        assert!(scan2.errors.is_empty(), "rescan must succeed: {:?}", scan2.errors);
        assert_eq!(scan2.files_processed, 1, "the modified file must be re-parsed");

        let bayerpat: Option<String> = conn
            .query_row(
                "SELECT f.bayerpat FROM frames f JOIN files fi ON fi.id = f.file_id
                 WHERE fi.path = ?1",
                [f.to_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bayerpat.as_deref(), Some("GRBG"), "frames column must follow disk");

        let header = stored_header(&conn);
        assert!(
            header.contains("BAYERPAT= 'GRBG"),
            "stored header blob must follow disk too, got: {header}"
        );
        assert!(
            !header.contains("BAYERPAT= 'RGGB"),
            "the previous ingest's CFA card must not survive in the blob"
        );

        // Exactly one blob row — the DELETE-then-INSERT is what enforces it
        // (fits_header has no UNIQUE(file_id)).
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fits_header h JOIN files fi ON fi.id = h.file_id
                 WHERE fi.path = ?1",
                [f.to_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1, "re-parse must not duplicate the stored header row");
    }

    /// Same invariant as `rescan_after_mtime_change_preserves_session_members`
    /// but exercises `scan_directory_parallel` — the path monitoring uses. That
    /// path used to DELETE + re-INSERT the rows in its classification phase,
    /// which broke every junction-table linkage; this pins that it goes through
    /// the in-place re-parse like the serial scan does.
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

    /// Audit C3: a `files` row with no `frames` row must never be classified
    /// as "unchanged". Size and mtime still match, so the old classification
    /// skipped it on every subsequent scan and the orphan was stuck forever.
    /// It must route through re-parse so the `frame_count == 0` self-heal in
    /// `reparse_and_update_in_place` recreates the frame, with `files.id`
    /// preserved.
    #[test]
    fn scan_heals_a_frameless_files_row() {
        use crate::events::NullEmitter;
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("L_001.fits");
        crate::archive::restore::tests::write_minimal_fits(&f);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [dir.path().to_str().unwrap()],
        )
        .unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let r1 = scan_directory_parallel(
            dir.path(),
            1,
            &conn,
            &NullEmitter,
            false,
            cancel.clone(),
            false,
        );
        assert!(r1.errors.is_empty(), "first scan clean: {:?}", r1.errors);
        let file_id: i64 = conn
            .query_row(
                "SELECT id FROM files WHERE filename = 'L_001.fits'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        // Simulate the historical C3 orphan: files row present, frames row gone.
        conn.execute("DELETE FROM frames WHERE file_id = ?1", [file_id])
            .unwrap();

        // Size and mtime are unchanged — the old classification skipped this
        // file forever. It must now be re-parsed and the frames row recreated,
        // with files.id preserved.
        let r2 = scan_directory_parallel(dir.path(), 1, &conn, &NullEmitter, false, cancel, false);
        assert!(r2.errors.is_empty(), "healing scan clean: {:?}", r2.errors);
        let frames: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM frames WHERE file_id = ?1",
                [file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(frames, 1, "frameless files row must be re-parsed");
    }

    /// Sequential sibling of `scan_heals_a_frameless_files_row`. `scan_directory`
    /// is production code (`api::scan_roots`, `calibration_library::register`),
    /// so the C3 guard has to hold on that path too.
    #[test]
    fn sequential_scan_heals_a_frameless_files_row() {
        let dir = TempDir::new().unwrap();
        let f = dir.path().join("L_004.fits");
        crate::archive::restore::tests::write_minimal_fits(&f);

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [dir.path().to_str().unwrap()],
        )
        .unwrap();

        let r1 = scan_directory(dir.path(), &conn, None, false, false, 1);
        assert!(r1.errors.is_empty(), "first scan clean: {:?}", r1.errors);
        let file_id: i64 = conn
            .query_row(
                "SELECT id FROM files WHERE filename = 'L_004.fits'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        // The historical C3 orphan: files row present, frames row gone, size
        // and mtime untouched.
        conn.execute("DELETE FROM frames WHERE file_id = ?1", [file_id])
            .unwrap();

        let r2 = scan_directory(dir.path(), &conn, None, false, false, 1);
        assert!(r2.errors.is_empty(), "healing scan clean: {:?}", r2.errors);
        assert_eq!(r2.files_skipped, 0, "the frameless row must not be skipped");
        let frames: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM frames WHERE file_id = ?1",
                [file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(frames, 1, "frameless files row must be re-parsed");
    }

    /// Scan-time content hashing is OPT-IN. `5aa58fed` hard-coded it on at both
    /// entry points, which added a 3 x 512 KB read per file — measured x5.9 on a
    /// cold 2010-file real library. The hash column is now populated by the
    /// content-index job instead (see `api::content_index`).
    #[test]
    fn scan_hashing_defaults_off_and_follows_the_setting() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let settings = crate::settings::SettingsManager::new();

        assert!(
            !scan_hashing_enabled(&settings, &conn),
            "no setting row: scan must not hash"
        );

        crate::db::set_setting(&conn, "duplicates.use_content_hash", "true").unwrap();
        assert!(
            scan_hashing_enabled(&settings, &conn),
            "setting on: scan hashes"
        );

        crate::db::set_setting(&conn, "duplicates.use_content_hash", "false").unwrap();
        assert!(
            !scan_hashing_enabled(&settings, &conn),
            "setting off: scan must not hash"
        );
    }

    /// The behavioural half: with hashing off the scanner writes NULL, so the
    /// content-index job has rows to find.
    #[test]
    fn scan_leaves_content_hash_null_when_hashing_is_off() {
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

        let cancel = Arc::new(AtomicBool::new(false));
        let result = scan_directory_parallel(
            scan.path(),
            1,
            &conn,
            &NullEmitter,
            false, // hashing off
            cancel,
            false,
        );
        assert_eq!(result.files_processed, 1);

        let hashed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE content_hash IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hashed, 0, "hashing off must leave content_hash NULL");
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
        let mut cal_dups = Vec::new();
        let result =
            process_file(&new_path, &conn, false, false, 2, &mut hash_errors, &mut cal_dups).unwrap();
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
        let mut cal_dups = Vec::new();
        let result =
            process_file(&new_path, &conn, false, false, 2, &mut hash_errors, &mut cal_dups).unwrap();
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

/// Scanner reconcile-adopt for calibrated-LIGHT artifacts (design §4). Each
/// test runs through BOTH scan entry points — the sequential `scan_directory`
/// and the parallel `scan_directory_parallel` (the production path) — since the
/// branch is implemented in both.
#[cfg(test)]
mod calibrated_light_scan_tests {
    use super::*;
    use crate::calibration_library::light_headers::{build_light_cal_cards, LightCalCardInputs};
    use crate::db::light_calibrations::{
        find_by_identity, upsert_light_calibration, FlatNormMode, LightCalRow,
        LIGHT_CAL_ENGINE_VERSION,
    };
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use crate::fits_writer::{write_fits_f32, Card, CardValue};
    use rusqlite::params;
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    /// Write a real calibrated-LIGHT FITS using the SAME header cards
    /// production writes (`build_light_cal_cards`, Task 2), so the scanner's
    /// detection path is exercised end to end.
    fn write_calibrated_light(
        path: &Path,
        source_uuid: &str,
        source_filename: &str,
        calstat: &str,
    ) {
        write_calibrated_light_full(
            path,
            source_uuid,
            source_filename,
            calstat,
            None,
            None,
            None,
            None,
            None,
        );
    }

    /// Full-fidelity calibrated-LIGHT writer: also stamps the copied-through
    /// OBJECT / DATE-OBS (filename disambiguation, §7) and the applied master
    /// references (`ATH_CDRK`/`ATH_CFLT`/`ATH_CBIA` as `"<uuid> <path>"`), all
    /// through the production `build_light_cal_cards`.
    #[allow(clippy::too_many_arguments)]
    fn write_calibrated_light_full(
        path: &Path,
        source_uuid: &str,
        source_filename: &str,
        calstat: &str,
        object: Option<&str>,
        date_obs: Option<&str>,
        dark: Option<(&str, &str)>,
        flat: Option<(&str, &str)>,
        bias: Option<(&str, &str)>,
    ) {
        // OBJECT / DATE-OBS ride in as source cards; build_light_cal_cards
        // copies them through its whitelist exactly as the engine would.
        let mut source_cards: Vec<Card> = Vec::new();
        if let Some(o) = object {
            source_cards.push(Card::new("OBJECT", CardValue::Str(o.to_string())).unwrap());
        }
        if let Some(d) = date_obs {
            source_cards.push(Card::new("DATE-OBS", CardValue::Str(d.to_string())).unwrap());
        }
        let owned = |p: Option<(&str, &str)>| p.map(|(u, path)| (u.to_string(), path.to_string()));
        let inputs = LightCalCardInputs {
            source_uuid: source_uuid.to_string(),
            source_filename: source_filename.to_string(),
            calstat: calstat.to_string(),
            dark: owned(dark),
            flat: owned(flat),
            bias: owned(bias),
            scale_divisor: 65535.0,
            flat_norm_divisor: 1.0,
            flat_channel_divisors: None,
            pedestal_dn: 0.0,
            trim_fraction: None,
        };
        let cards = build_light_cal_cards(&source_cards, &inputs).unwrap();
        let data = vec![0.5f32; 4 * 4];
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_fits_f32(path, 4, 4, 1, &data, &cards).unwrap();
    }

    /// Seed a master calibration set (`is_master_library`) with one master
    /// frame at (`id`, `id`), carrying `uuid` on both its file and frame and
    /// `path` on its file — so an adopted light's master ref can resolve either
    /// by uuid (frame) or by path (file). Returns the calibration_set id.
    fn seed_master_set(
        conn: &Connection,
        id: i64,
        uuid: &str,
        path: &str,
        imagetyp: &str,
    ) -> i64 {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, uuid)
             VALUES (?1, ?2, 'master.fits', 0, '2025-01-01T00:00:00Z', 'FITS', ?3)",
            params![id, path, uuid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, uuid, is_master) VALUES (?1, ?1, ?2, 1)",
            params![id, uuid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, is_master_library) VALUES (?1, '2025-01-01', 1)",
            params![imagetyp],
        )
        .unwrap();
        let set_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            params![set_id, id],
        )
        .unwrap();
        set_id
    }

    fn fresh_db(root: &Path, root_id: i64) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (?1, ?2)",
            params![root_id, root.to_str().unwrap()],
        )
        .unwrap();
        conn
    }

    /// Seed a `light_calibrations` tracking row (adopted-row shape, keyed on
    /// output_path) with a known identity.
    fn seed_row(conn: &Connection, source_uuid: &str, output_path: &str) {
        let row = LightCalRow {
            id: 0,
            frame_id: None,
            source_uuid: Some(source_uuid.to_string()),
            source_filename: None,
            output_path: output_path.to_string(),
            dark_set_id: None,
            flat_set_id: None,
            bias_set_id: None,
            calstat: "BD".to_string(),
            flat_norm_applied: false,
            flat_norm_mode: FlatNormMode::CentralThird.as_wire_str().to_string(),
            cal_params: "{}".to_string(),
            cfa_scaling_applied: None,
            output_hash: "seed".to_string(),
            engine_version: LIGHT_CAL_ENGINE_VERSION,
            created_at: "2026-07-05T00:00:00Z".to_string(),
        };
        upsert_light_calibration(conn, &row).unwrap();
    }

    fn run_scan(parallel: bool, root: &Path, conn: &Connection, root_id: i64) -> ScanResult {
        if parallel {
            scan_directory_parallel(
                root,
                root_id,
                conn,
                &NullEmitter,
                false,
                Arc::new(AtomicBool::new(false)),
                false,
            )
        } else {
            scan_directory(root, conn, None, false, false, root_id)
        }
    }

    fn files_count(conn: &Connection, path: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM files WHERE path = ?1",
            params![path],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn light_cal_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM light_calibrations", [], |r| r.get(0))
            .unwrap()
    }

    /// Branch 1: the tracking row's output_path equals the scanned path — no-op.
    /// The file is never registered as a frame and the row is untouched.
    #[test]
    fn scan_skips_known_calibrated_light() {
        for parallel in [false, true] {
            let root = TempDir::new().unwrap();
            let cal = root.path().join("M42/c_L_0001.fits");
            write_calibrated_light(&cal, "uuid-known", "L_0001.fits", "BDF");
            let cal_str = cal.to_str().unwrap().to_string();

            let conn = fresh_db(root.path(), 1);
            seed_row(&conn, "uuid-known", &cal_str);

            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
            assert_eq!(files_count(&conn, &cal_str), 0, "parallel={parallel}: never registered");
            assert_eq!(light_cal_count(&conn), 1, "parallel={parallel}: no new row");
            assert!(
                result.calibrated_duplicates.is_empty(),
                "parallel={parallel}: known path is not a duplicate"
            );
            let row = find_by_identity(&conn, Some("uuid-known"), None).unwrap().unwrap();
            assert_eq!(row.output_path, cal_str, "parallel={parallel}: output_path unchanged");
        }
    }

    /// Branch 2: identity matches a row whose output_path no longer exists on
    /// disk — repair the pointer to the new location.
    #[test]
    fn scan_repairs_moved_calibrated_light() {
        for parallel in [false, true] {
            let root = TempDir::new().unwrap();
            let cal = root.path().join("M42/c_L_0002.fits");
            write_calibrated_light(&cal, "uuid-moved", "L_0002.fits", "BD");
            let cal_str = cal.to_str().unwrap().to_string();

            let conn = fresh_db(root.path(), 1);
            // Old output_path points somewhere that does NOT exist on disk.
            let old_path = root.path().join("old/gone/c_L_0002.fits");
            seed_row(&conn, "uuid-moved", old_path.to_str().unwrap());

            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
            assert_eq!(files_count(&conn, &cal_str), 0, "parallel={parallel}: never registered");
            assert!(result.calibrated_duplicates.is_empty(), "parallel={parallel}: a move is not a dup");

            let row = find_by_identity(&conn, Some("uuid-moved"), None).unwrap().unwrap();
            assert_eq!(row.output_path, cal_str, "parallel={parallel}: output_path repaired to new location");
            assert_eq!(light_cal_count(&conn), 1, "parallel={parallel}: repaired in place, no new row");
        }
    }

    /// Branch 3: identity matches a row whose output_path ALSO still exists — a
    /// duplicate copy. Record the pair; leave the tracking row untouched.
    #[test]
    fn scan_reports_duplicate_copy() {
        for parallel in [false, true] {
            let root = TempDir::new().unwrap();
            // The kept (tracked) file lives OUTSIDE the scan root and stays on disk.
            let kept_dir = TempDir::new().unwrap();
            let kept = kept_dir.path().join("c_L_0003.fits");
            write_calibrated_light(&kept, "uuid-dup", "L_0003.fits", "BDF");
            let kept_str = kept.to_str().unwrap().to_string();

            // The duplicate copy sits inside the scan root.
            let dup = root.path().join("M42/c_L_0003.fits");
            write_calibrated_light(&dup, "uuid-dup", "L_0003.fits", "BDF");
            let dup_str = dup.to_str().unwrap().to_string();

            let conn = fresh_db(root.path(), 1);
            seed_row(&conn, "uuid-dup", &kept_str);

            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
            assert_eq!(files_count(&conn, &dup_str), 0, "parallel={parallel}: dup never registered");
            assert_eq!(
                result.calibrated_duplicates,
                vec![CalibratedDuplicate { kept_path: kept_str.clone(), duplicate_path: dup_str.clone() }],
                "parallel={parallel}: duplicate pair reported"
            );
            // Row untouched: still points at the kept file.
            let row = find_by_identity(&conn, Some("uuid-dup"), None).unwrap().unwrap();
            assert_eq!(row.output_path, kept_str, "parallel={parallel}: tracking row unchanged");
        }
    }

    /// Branch 2b: the stored output_path and the scanned path are lexically
    /// different but canonicalize to the SAME physical file (Windows
    /// trailing-dot/case normalization, symlinked mounts). Repair the pointer
    /// to the scanned spelling — branch 3 would otherwise flag it as a phantom
    /// duplicate on every scan, forever. Called directly: the normalization is
    /// a filesystem property a scan walk on a case-sensitive fs can't reproduce.
    #[test]
    fn reconcile_repairs_row_when_stored_and_scanned_paths_are_same_file() {
        // Cross-platform stand-in for Windows normalization: a lexically
        // different but canonically identical spelling of one path (`M31/../M31/…`).
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("M31");
        std::fs::create_dir_all(&sub).unwrap();
        let real = sub.join("c_L_0001.fits");
        std::fs::write(&real, b"stub").unwrap();
        let detoured = dir.path().join("M31").join("..").join("M31").join("c_L_0001.fits");

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        upsert_light_calibration(
            &conn,
            &LightCalRow {
                id: 0,
                frame_id: None,
                source_uuid: Some("uuid-same-file".into()),
                source_filename: Some("L_0001.fits".into()),
                output_path: detoured.to_string_lossy().to_string(),
                dark_set_id: None,
                flat_set_id: None,
                bias_set_id: None,
                calstat: "D".into(),
                flat_norm_applied: false,
                flat_norm_mode: "centralThird".into(),
                output_hash: "h".into(),
                engine_version: LIGHT_CAL_ENGINE_VERSION,
                created_at: "2026-01-01T00:00:00Z".into(),
                cal_params: "{}".into(),
                cfa_scaling_applied: None,
            },
        )
        .unwrap();

        let identity = CalibratedIdentity {
            source_uuid: Some("uuid-same-file".into()),
            source_filename: Some("L_0001.fits".into()),
            source_object: None,
            source_date_obs: None,
            calstat: "D".into(),
            dark: None,
            flat: None,
            bias: None,
            flat_norm_divisor: None,
            cfa_scaling_applied: None,
            engine_version: None,
            project_id: None,
        };
        let current = real.to_string_lossy().to_string();
        let mut dups = Vec::new();
        reconcile_calibrated_light(&conn, &real, &current, &identity, 1, &mut dups).unwrap();

        assert!(dups.is_empty(), "same physical file must not be flagged duplicate");
        let stored: String = conn
            .query_row(
                "SELECT output_path FROM light_calibrations WHERE source_uuid = 'uuid-same-file'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, current, "output_path repaired to the scanned spelling");
    }

    /// Branch 4 (resolved): no tracking row, but the source frame is cataloged
    /// by filename — adopt: INSERT a tracking row pointing at the source frame.
    #[test]
    fn scan_adopts_after_db_rebuild() {
        for parallel in [false, true] {
            let root = TempDir::new().unwrap();
            let cal = root.path().join("M42/c_L_src.fits");
            // Empty uuid -> adoption falls back to the filename (§4).
            write_calibrated_light(&cal, "", "L_src.fits", "BDF");
            let cal_str = cal.to_str().unwrap().to_string();

            let conn = fresh_db(root.path(), 1);
            // Cataloged source frame with the referenced filename.
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (900, '/src/L_src.fits', 'L_src.fits', 0, '2025-01-01T00:00:00Z', 'FITS')",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO frames (id, file_id) VALUES (900, 900)", []).unwrap();

            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
            assert_eq!(files_count(&conn, &cal_str), 0, "parallel={parallel}: artifact never registered");

            let (count, output_path): (i64, Option<String>) = conn
                .query_row(
                    "SELECT COUNT(*), MAX(output_path) FROM light_calibrations WHERE frame_id = 900",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(count, 1, "parallel={parallel}: adopted row created for source frame");
            assert_eq!(output_path.as_deref(), Some(cal_str.as_str()), "parallel={parallel}: output_path = artifact");
        }
    }

    /// Branch 4 (unresolved): no tracking row and the source frame is NOT
    /// cataloged — defer. No row is inserted and the artifact is never
    /// registered, so a later scan (once the source lands) can still adopt.
    #[test]
    fn scan_defers_adoption_when_source_missing() {
        for parallel in [false, true] {
            let root = TempDir::new().unwrap();
            let cal = root.path().join("M42/c_L_missing.fits");
            write_calibrated_light(&cal, "", "L_missing.fits", "BDF");
            let cal_str = cal.to_str().unwrap().to_string();

            let conn = fresh_db(root.path(), 1);
            // No source frame cataloged.

            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
            assert_eq!(files_count(&conn, &cal_str), 0, "parallel={parallel}: artifact never registered");
            assert_eq!(light_cal_count(&conn), 0, "parallel={parallel}: adoption deferred, no row");
            assert!(result.calibrated_duplicates.is_empty(), "parallel={parallel}");
        }
    }

    /// Finding 1 (resolved): two catalog frames share the filename `L_0001.fits`
    /// (post-rebuild uuids gone, so resolution is by filename). The calibrated
    /// file carries OBJECT + DATE-OBS matching exactly ONE of them → adopt that
    /// frame, never the collision. DATE-OBS is carried verbatim (no timezone)
    /// while the DB row is RFC3339, exercising the instant-aware compare.
    #[test]
    fn scan_adopts_disambiguates_same_filename_by_object_and_date() {
        for parallel in [false, true] {
            let root = TempDir::new().unwrap();
            let cal = root.path().join("M42/c_L_0001.fits");
            write_calibrated_light_full(
                &cal,
                "", // empty uuid -> filename fallback
                "L_0001.fits",
                "BDF",
                Some("M42"),
                Some("2025-01-01T22:00:00"),
                None,
                None,
                None,
            );
            let cal_str = cal.to_str().unwrap().to_string();

            let conn = fresh_db(root.path(), 1);
            // TWO frames collide on the filename; only #900 matches OBJECT+DATE.
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (900, '/a/L_0001.fits', 'L_0001.fits', 0, '2025-01-01T00:00:00Z', 'FITS'),
                        (901, '/b/L_0001.fits', 'L_0001.fits', 0, '2025-01-01T00:00:00Z', 'FITS')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, object, date_obs) VALUES
                    (900, 900, 'M42', '2025-01-01T22:00:00+00:00'),
                    (901, 901, 'M81', '2025-02-01T22:00:00+00:00')",
                [],
            )
            .unwrap();

            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
            assert_eq!(files_count(&conn, &cal_str), 0, "parallel={parallel}: artifact never registered");

            let frame_id: i64 = conn
                .query_row(
                    "SELECT frame_id FROM light_calibrations WHERE output_path = ?1",
                    params![cal_str],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(frame_id, 900, "parallel={parallel}: adopted the OBJECT+DATE match, not the collision");
            assert_eq!(light_cal_count(&conn), 1, "parallel={parallel}: exactly one adopted row");
        }
    }

    /// Finding 1 (ambiguous): two catalog frames share the filename and the
    /// calibrated file carries NO disambiguating cards → adoption is deferred
    /// (no arbitrary bind, no row-collapse). No row inserted, not registered;
    /// a later scan adopts once the catalog disambiguates.
    #[test]
    fn scan_defers_adoption_when_filename_ambiguous() {
        for parallel in [false, true] {
            let root = TempDir::new().unwrap();
            let cal = root.path().join("M42/c_L_amb.fits");
            // No OBJECT / DATE-OBS -> filename alone can't break the tie.
            write_calibrated_light(&cal, "", "L_amb.fits", "BDF");
            let cal_str = cal.to_str().unwrap().to_string();

            let conn = fresh_db(root.path(), 1);
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (900, '/a/L_amb.fits', 'L_amb.fits', 0, '2025-01-01T00:00:00Z', 'FITS'),
                        (901, '/b/L_amb.fits', 'L_amb.fits', 0, '2025-01-01T00:00:00Z', 'FITS')",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO frames (id, file_id) VALUES (900, 900), (901, 901)", [])
                .unwrap();

            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
            assert_eq!(files_count(&conn, &cal_str), 0, "parallel={parallel}: artifact never registered");
            assert_eq!(light_cal_count(&conn), 0, "parallel={parallel}: ambiguous source -> deferred, no row");
        }
    }

    /// Finding 2: master-reference resolution through an adopt scan. The
    /// calibrated fixture carries a dark ref (uuid hit), a flat ref (uuid miss
    /// but path hit → path fallback), and a bias ref (both miss → NULL). Assert
    /// the adopted row's dark/flat/bias set ids.
    #[test]
    fn scan_adopts_resolves_master_references() {
        for parallel in [false, true] {
            let root = TempDir::new().unwrap();
            let conn = fresh_db(root.path(), 1);

            // Source LIGHT frame the calibrated artifact was built from.
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (900, '/src/L_m.fits', 'L_m.fits', 0, '2025-01-01T00:00:00Z', 'FITS')",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO frames (id, file_id) VALUES (900, 900)", []).unwrap();

            // Seeded master sets: dark resolvable by uuid, flat by path.
            let dark_set = seed_master_set(&conn, 800, "dark-uuid-1", "/lib/master_dark.fits", "Dark");
            let flat_set = seed_master_set(&conn, 801, "flat-uuid-real", "/lib/master_flat.fits", "Flat");

            let cal = root.path().join("M42/c_L_m.fits");
            write_calibrated_light_full(
                &cal,
                "",
                "L_m.fits",
                "BDF",
                None,
                None,
                Some(("dark-uuid-1", "/lib/master_dark.fits")), // uuid hit
                Some(("flat-uuid-WRONG", "/lib/master_flat.fits")), // uuid miss, path hit
                Some(("bias-uuid-unknown", "/lib/nope_bias.fits")), // both miss -> NULL
            );
            let cal_str = cal.to_str().unwrap().to_string();

            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);

            let (dark_id, flat_id, bias_id): (Option<i64>, Option<i64>, Option<i64>) = conn
                .query_row(
                    "SELECT dark_set_id, flat_set_id, bias_set_id
                     FROM light_calibrations WHERE output_path = ?1",
                    params![cal_str],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(dark_id, Some(dark_set), "parallel={parallel}: dark resolved by uuid");
            assert_eq!(flat_id, Some(flat_set), "parallel={parallel}: flat resolved by path fallback");
            assert_eq!(bias_id, None, "parallel={parallel}: unresolvable bias ref -> NULL");
        }
    }

    /// Seed a cataloged source LIGHT frame (files + frames) carrying a specific
    /// filename / OBJECT / DATE-OBS, for filename-collision tests.
    fn seed_source_frame(conn: &Connection, frame_id: i64, filename: &str, object: &str, date_obs: &str) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2025-01-01T00:00:00Z', 'FITS')",
            params![frame_id, format!("/src/{frame_id}/{filename}"), filename],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, object, date_obs) VALUES (?1, ?1, ?2, ?3)",
            params![frame_id, object, date_obs],
        )
        .unwrap();
    }

    /// Seed a tracking row bound to a cataloged source frame (keyed on frame_id),
    /// carrying a `source_filename` and `output_path`.
    fn seed_tracking_row(conn: &Connection, frame_id: i64, source_filename: &str, output_path: &str) {
        let row = LightCalRow {
            id: 0,
            frame_id: Some(frame_id),
            source_uuid: None,
            source_filename: Some(source_filename.to_string()),
            output_path: output_path.to_string(),
            dark_set_id: None,
            flat_set_id: None,
            bias_set_id: None,
            calstat: "BD".to_string(),
            flat_norm_applied: false,
            flat_norm_mode: FlatNormMode::CentralThird.as_wire_str().to_string(),
            cal_params: "{}".to_string(),
            cfa_scaling_applied: None,
            output_hash: "seed".to_string(),
            engine_version: LIGHT_CAL_ENGINE_VERSION,
            created_at: "2026-07-05T00:00:00Z".to_string(),
        };
        upsert_light_calibration(conn, &row).unwrap();
    }

    /// F2 (branch 3): a tracking row for object M81 and a calibrated file for
    /// object M42 share the filename `L_0001.fits`. Scanning the M42 file must
    /// NOT branch-3-duplicate it against the M81 row (which the bare-filename
    /// match did) — it belongs to a different source frame — but instead adopt a
    /// fresh row for the true M42 owner.
    #[test]
    fn scan_no_cross_object_duplicate_on_filename_collision() {
        for parallel in [false, true] {
            let root = TempDir::new().unwrap();
            let conn = fresh_db(root.path(), 1);

            // Two cataloged source frames collide on the filename.
            seed_source_frame(&conn, 900, "L_0001.fits", "M42", "2025-01-01T22:00:00+00:00");
            seed_source_frame(&conn, 901, "L_0001.fits", "M81", "2025-02-01T22:00:00+00:00");

            // The M81 tracking row points at a file that STILL exists on disk
            // (outside the scan root) — a bare-filename match would branch-3 it.
            let kept_dir = TempDir::new().unwrap();
            let kept_m81 = kept_dir.path().join("c_m81.fits");
            write_calibrated_light(&kept_m81, "", "L_0001.fits", "BDF");
            let kept_m81_str = kept_m81.to_str().unwrap().to_string();
            seed_tracking_row(&conn, 901, "L_0001.fits", &kept_m81_str);

            // The calibrated M42 file lands in the scan root (object/date = M42's).
            let cal = root.path().join("M42/c_L_0001.fits");
            write_calibrated_light_full(
                &cal, "", "L_0001.fits", "BDF",
                Some("M42"), Some("2025-01-01T22:00:00"), None, None, None,
            );
            let cal_str = cal.to_str().unwrap().to_string();

            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
            assert!(
                result.calibrated_duplicates.is_empty(),
                "parallel={parallel}: must not cross-object duplicate against the M81 row"
            );
            // The M42 file was adopted for its true owner (frame 900).
            let adopted: i64 = conn
                .query_row(
                    "SELECT frame_id FROM light_calibrations WHERE output_path = ?1",
                    params![cal_str],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(adopted, 900, "parallel={parallel}: adopted the M42 owner");
            // The M81 row is untouched.
            let m81_path: String = conn
                .query_row(
                    "SELECT output_path FROM light_calibrations WHERE frame_id = 901",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(m81_path, kept_m81_str, "parallel={parallel}: M81 row unchanged");
            assert_eq!(light_cal_count(&conn), 2, "parallel={parallel}: M81 row + adopted M42 row");
        }
    }

    /// F2 (branch 2): two tracking rows share the filename `L_0001.fits` for
    /// different objects — M42's output is gone from disk, M81's is present.
    /// Scanning the M42 file must repair ONLY the M42 row (its true owner),
    /// never the M81 row, and never emit a duplicate.
    #[test]
    fn scan_repairs_only_true_owner_on_filename_collision() {
        for parallel in [false, true] {
            let root = TempDir::new().unwrap();
            let conn = fresh_db(root.path(), 1);

            seed_source_frame(&conn, 900, "L_0001.fits", "M42", "2025-01-01T22:00:00+00:00");
            seed_source_frame(&conn, 901, "L_0001.fits", "M81", "2025-02-01T22:00:00+00:00");

            // M42 row's output is GONE from disk (repair candidate)...
            let gone_m42 = root.path().join("old/gone/c_m42.fits");
            seed_tracking_row(&conn, 900, "L_0001.fits", gone_m42.to_str().unwrap());
            // ...M81 row's output is PRESENT (must stay untouched).
            let kept_dir = TempDir::new().unwrap();
            let present_m81 = kept_dir.path().join("c_m81.fits");
            write_calibrated_light(&present_m81, "", "L_0001.fits", "BDF");
            let present_m81_str = present_m81.to_str().unwrap().to_string();
            seed_tracking_row(&conn, 901, "L_0001.fits", &present_m81_str);

            // The scanned M42 file carries M42's OBJECT/DATE-OBS.
            let cal = root.path().join("M42/c_L_0001.fits");
            write_calibrated_light_full(
                &cal, "", "L_0001.fits", "BD",
                Some("M42"), Some("2025-01-01T22:00:00"), None, None, None,
            );
            let cal_str = cal.to_str().unwrap().to_string();

            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
            assert!(
                result.calibrated_duplicates.is_empty(),
                "parallel={parallel}: a same-owner move must not be a duplicate"
            );
            // M42 row repaired to the scanned file...
            let m42_path: String = conn
                .query_row(
                    "SELECT output_path FROM light_calibrations WHERE frame_id = 900",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(m42_path, cal_str, "parallel={parallel}: M42 row repaired to new location");
            // ...M81 row untouched.
            let m81_path: String = conn
                .query_row(
                    "SELECT output_path FROM light_calibrations WHERE frame_id = 901",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(m81_path, present_m81_str, "parallel={parallel}: M81 row unchanged");
            assert_eq!(light_cal_count(&conn), 2, "parallel={parallel}: repaired in place, no new row");
        }
    }

    // -----------------------------------------------------------------------
    // Project-contribution reconcile (slice 4, spec §7): a stamped received
    // contribution carries ATH_PRJ on top of the calibrated-light cards, so the
    // scanner diverts it to `reconcile_project_contribution` (sibling of the
    // light-cal reconcile) — it NEVER enters `files`/`frames` on any branch.
    // -----------------------------------------------------------------------

    use crate::db::collab_exchange::{
        find_contribution_by_landed_path, insert_contribution, upsert_package, ContributionRow,
        PackageRow,
    };

    /// Write a project-contribution FITS: the SAME calibrated-light cards
    /// production stamps, PLUS the `ATH_PRJ` project card publish appends. The
    /// file therefore carries CALSTAT + ATH_CSRC + ATH_PRJ.
    fn write_project_contribution(path: &Path, source_uuid: &str, filename: &str, project_id: &str) {
        let inputs = LightCalCardInputs {
            source_uuid: source_uuid.to_string(),
            source_filename: filename.to_string(),
            calstat: "BDF".to_string(),
            dark: None,
            flat: None,
            bias: None,
            scale_divisor: 65535.0,
            flat_norm_divisor: 1.0,
            flat_channel_divisors: None,
            pedestal_dn: 0.0,
            trim_fraction: None,
        };
        let source_cards: Vec<Card> = Vec::new();
        let mut cards = build_light_cal_cards(&source_cards, &inputs).unwrap();
        cards.push(Card::new("ATH_PRJ", CardValue::Str(project_id.to_string())).unwrap());
        let data = vec![0.5f32; 4 * 4];
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_fits_f32(path, 4, 4, 1, &data, &cards).unwrap();
    }

    /// Minimal parent package row (the contribution FK target).
    fn seed_package(conn: &Connection, package_id: &str, project_id: &str) {
        let row = PackageRow {
            package_id: package_id.to_string(),
            project_id: project_id.to_string(),
            announcement_id: format!("ann-{package_id}"),
            publisher_display: "Alice".into(),
            own: false,
            root_hash: "rh".into(),
            byte_size: 0,
            frame_count: 0,
            manifest_xxh3: None,
            aggregate_stats: "{}".into(),
            supersedes: "[]".into(),
            state: "published".into(),
            reject_reason: None,
            superseded: false,
            origin: "received".into(),
            local_dir: None,
            manifest_ndjson: None,
            local_status: "complete".into(),
            holder_count: 0,
            online_count: 0,
            created_at: "2026-07-14 00:00:00".into(),
            decided_at: None,
            fetched_at: String::new(),
        };
        upsert_package(conn, &row).unwrap();
    }

    /// Seed a contribution tracking row (parent package auto-seeded) with a
    /// specific `landed_path` and full-content `xxh3`.
    fn seed_contribution(
        conn: &Connection,
        project_id: &str,
        package_id: &str,
        landed_path: &str,
        xxh3: &str,
    ) {
        seed_package(conn, package_id, project_id);
        let row = ContributionRow {
            id: 0,
            project_id: project_id.to_string(),
            package_id: package_id.to_string(),
            frame_uuid: "u-1".into(),
            publisher_display: "Alice".into(),
            rel_path: "Alice/c.fits".into(),
            landed_path: landed_path.to_string(),
            byte_size: 0,
            xxh3: xxh3.to_string(),
            frame_meta: "{}".into(),
            analysis: None,
            superseded: false,
            created_at: String::new(),
        };
        insert_contribution(conn, &row).unwrap();
    }

    fn contributions_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM project_contributions", [], |r| r.get(0))
            .unwrap()
    }

    /// Known: a tracking row's `landed_path` equals the scanned path — no-op.
    /// Never registered as a frame, tracking row untouched.
    #[test]
    fn scan_skips_known_project_contribution() {
        for parallel in [false, true] {
            let root = TempDir::new().unwrap();
            let cal = root.path().join("proj/Alice/c_L_0001.fits");
            write_project_contribution(&cal, "uuid-k", "L_0001.fits", "proj-1");
            let cal_str = cal.to_str().unwrap().to_string();
            let xxh3 = crate::package::xxh3_full_file(&cal).unwrap();

            let conn = fresh_db(root.path(), 1);
            seed_contribution(&conn, "proj-1", "pkg-1", &cal_str, &xxh3);

            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
            assert_eq!(files_count(&conn, &cal_str), 0, "parallel={parallel}: never registered");
            assert_eq!(light_cal_count(&conn), 0, "parallel={parallel}: not a light-cal adoption");
            assert_eq!(contributions_count(&conn), 1, "parallel={parallel}: no new tracking row");
            let row = find_contribution_by_landed_path(&conn, &cal_str).unwrap().unwrap();
            assert_eq!(row.landed_path, cal_str, "parallel={parallel}: landed_path unchanged");
        }
    }

    /// Moved: a row matches `(project_id, xxh3)` and its `landed_path` is gone
    /// from disk — repair the pointer to the new location.
    #[test]
    fn scan_repairs_moved_project_contribution() {
        for parallel in [false, true] {
            let root = TempDir::new().unwrap();
            let cal = root.path().join("proj/Alice/c_L_0002.fits");
            write_project_contribution(&cal, "uuid-m", "L_0002.fits", "proj-1");
            let cal_str = cal.to_str().unwrap().to_string();
            let xxh3 = crate::package::xxh3_full_file(&cal).unwrap();

            let conn = fresh_db(root.path(), 1);
            // Old landed_path does not exist on disk.
            let gone = root.path().join("old/gone/c_L_0002.fits");
            seed_contribution(&conn, "proj-1", "pkg-1", gone.to_str().unwrap(), &xxh3);

            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
            assert_eq!(files_count(&conn, &cal_str), 0, "parallel={parallel}: never registered");
            assert_eq!(contributions_count(&conn), 1, "parallel={parallel}: repaired in place");
            let row = find_contribution_by_landed_path(&conn, &cal_str).unwrap().unwrap();
            assert_eq!(row.landed_path, cal_str, "parallel={parallel}: landed_path repaired");
        }
    }

    /// Duplicate: a row matches `(project_id, xxh3)` but its `landed_path` STILL
    /// exists — this is a second copy. Leave the row untouched; never register.
    #[test]
    fn scan_leaves_duplicate_project_contribution() {
        for parallel in [false, true] {
            let root = TempDir::new().unwrap();
            // Kept (tracked) copy lives OUTSIDE the scan root and stays on disk.
            let kept_dir = TempDir::new().unwrap();
            let kept = kept_dir.path().join("c_L_0003.fits");
            write_project_contribution(&kept, "uuid-d", "L_0003.fits", "proj-1");
            let kept_str = kept.to_str().unwrap().to_string();
            let xxh3 = crate::package::xxh3_full_file(&kept).unwrap();

            // The duplicate copy sits inside the scan root (byte-identical).
            let dup = root.path().join("proj/Alice/c_L_0003.fits");
            write_project_contribution(&dup, "uuid-d", "L_0003.fits", "proj-1");
            let dup_str = dup.to_str().unwrap().to_string();

            let conn = fresh_db(root.path(), 1);
            seed_contribution(&conn, "proj-1", "pkg-1", &kept_str, &xxh3);

            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
            assert_eq!(files_count(&conn, &dup_str), 0, "parallel={parallel}: dup never registered");
            assert_eq!(contributions_count(&conn), 1, "parallel={parallel}: no new tracking row");
            let row = find_contribution_by_landed_path(&conn, &kept_str).unwrap().unwrap();
            assert_eq!(row.landed_path, kept_str, "parallel={parallel}: tracking row unchanged");
            assert!(
                find_contribution_by_landed_path(&conn, &dup_str).unwrap().is_none(),
                "parallel={parallel}: the duplicate copy is not tracked"
            );
        }
    }

    /// Unknown: no tracking row (hand-dropped stamped file, or the package is
    /// gone) — leave it untouched and defer. Nothing enters `files`/`frames`,
    /// and NO contribution row is auto-inserted (a row needs hub context).
    #[test]
    fn scan_leaves_unknown_project_contribution() {
        for parallel in [false, true] {
            let root = TempDir::new().unwrap();
            let cal = root.path().join("proj/Alice/c_L_orphan.fits");
            write_project_contribution(&cal, "uuid-o", "L_orphan.fits", "proj-1");
            let cal_str = cal.to_str().unwrap().to_string();

            // No package, no contribution rows at all.
            let conn = fresh_db(root.path(), 1);
            // A cataloged source frame the light-cal adopt path COULD grab —
            // proves the ATH_PRJ divert wins and never falls through to adoption.
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (900, '/src/L_orphan.fits', 'L_orphan.fits', 0, '2025-01-01T00:00:00Z', 'FITS')",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO frames (id, file_id) VALUES (900, 900)", []).unwrap();

            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
            assert_eq!(files_count(&conn, &cal_str), 0, "parallel={parallel}: never registered");
            assert_eq!(
                light_cal_count(&conn),
                0,
                "parallel={parallel}: ATH_PRJ divert wins — no light-cal adoption"
            );
            assert_eq!(contributions_count(&conn), 0, "parallel={parallel}: no auto-inserted row");
            // The only file row is the pre-seeded source frame; the artifact is absent.
            let total_files: i64 =
                conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
            assert_eq!(total_files, 1, "parallel={parallel}: artifact contributed no files row");
        }
    }
}
