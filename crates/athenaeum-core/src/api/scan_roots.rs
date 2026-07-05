//! Shared `scan_roots` command-layer handlers — single business-logic source
//! for the Tauri (`commands/scan_roots.rs`) and web (`routes/scan_roots.rs`)
//! wrappers. See `.superpowers/sdd/p1-task-9-brief.md` for the conversion
//! recipe this module follows (reused verbatim by Tasks 10-12).
//!
//! Desktop bodies are authoritative; web-only divergences (path sandboxing,
//! HTTP-status-specific error classification) are folded in as `PathPolicy`
//! checks / `ApiError` variants per the recipe. See the Task 9 report for the
//! full drift catalog.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;

use crate::api::{db, ApiError, PathPolicy};
use crate::monitor::MonitorService;
use crate::models::{OrphanedFile, RelinkResult, ScanRoot};
use crate::services::ServiceContext;

// ── Response DTOs (single-sourced; both wrapper crates import these) ────────

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

// ── Path helper ──────────────────────────────────────────────────────────────

/// Strip Windows extended-length path prefix that `canonicalize()` adds.
/// Handles three cases:
///   \\?\C:\...        -> C:\...          (local path)
///   \\?\UNC\server\.. -> \\server\..     (network UNC path)
///   \\server\share\.. -> unchanged       (regular UNC, no prefix)
/// On non-Windows platforms, this is a no-op.
///
/// Moved from `athenaeum-tauri::commands::utils::normalize_path` (only ever
/// called from `add_scan_root`) — pure `std::path`, no Tauri dependency.
fn normalize_path(path: &Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let s = path.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
            return std::path::PathBuf::from(format!(r"\\{}", stripped));
        }
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return std::path::PathBuf::from(stripped);
        }
    }
    path.to_path_buf()
}

// ── Handlers ─────────────────────────────────────────────────────────────────

pub fn get_scan_roots(ctx: &ServiceContext) -> Result<Vec<ScanRoot>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_scan_roots(&conn)?)
}

/// Validate a scan-root `kind` value: `"normal"` | `"calibration_library"`,
/// anything else is `ApiError::Invalid`. Runs at the top of `add_scan_root`,
/// before any path/DB work.
pub(crate) fn validate_scan_root_kind(kind: &str) -> Result<(), ApiError> {
    if kind != "normal" && kind != "calibration_library" {
        return Err(ApiError::Invalid(format!("unknown scan root kind: {kind}")));
    }
    Ok(())
}

/// Single-library-root enforcement: adding a `"calibration_library"` root
/// when one already exists is `ApiError::Conflict`. A no-op for `"normal"`.
/// Runs in `add_scan_root` after overlap validation, before the DB write.
pub(crate) fn check_library_root_uniqueness(
    conn: &rusqlite::Connection,
    kind: &str,
) -> Result<(), ApiError> {
    if kind == "calibration_library"
        && crate::db::count_scan_roots_of_kind(conn, "calibration_library")? > 0
    {
        return Err(ApiError::Conflict(
            "A Calibration Library root already exists — only one is allowed".to_string(),
        ));
    }
    Ok(())
}

/// Validates the path exists, canonicalizes it (resolving `..`/symlinks),
/// sandboxes it against `policy` (desktop: `AllowAll`; web: `AllowedRoots`
/// built from the caller's *canonicalized* allowed roots — see
/// `PathPolicy::check`'s doc comment for the precondition), checks for
/// overlaps with existing roots, then inserts via `db::upsert_scan_root`.
///
/// Overlap/duplicate cases return `ApiError::Conflict` — matching the web
/// route's pre-conversion `StatusCode::CONFLICT` mapping.
///
/// `kind` defaults to `"normal"` when `None`. `"calibration_library"` is
/// enforced unique across all scan roots (`ApiError::Conflict` if another
/// library root already exists); any other value is `ApiError::Invalid`.
/// Note: the overlap checks below reject a new root that is a subdirectory
/// of an existing one — so a calibration_library root nested inside an
/// existing scan root is rejected too. That's intentional: the library is
/// itself just a normal scanned root, so it must live outside every other
/// registered root rather than inside one.
///
/// The kind checks themselves live in `validate_scan_root_kind` /
/// `check_library_root_uniqueness` (extracted so they're testable at the
/// `Connection` level without a full `ServiceContext`).
pub fn add_scan_root(
    ctx: &ServiceContext,
    path: String,
    policy: &PathPolicy,
    kind: Option<String>,
) -> Result<ScanRoot, ApiError> {
    let kind = kind.unwrap_or_else(|| "normal".to_string());
    validate_scan_root_kind(&kind)?;

    let db = db(ctx)?;
    let conn = db.conn();

    // 1. Check if directory exists
    let path_buf = Path::new(&path);
    if !path_buf.exists() {
        return Err(ApiError::Invalid("Directory does not exist".to_string()));
    }
    if !path_buf.is_dir() {
        return Err(ApiError::Invalid("Path is not a directory".to_string()));
    }

    // 2. Canonicalize the new path (resolve symlinks, .., etc.)
    //    normalize_path strips the \\?\ prefix that Windows canonicalize() adds
    let new_path = normalize_path(
        &path_buf
            .canonicalize()
            .map_err(|e| ApiError::Internal(format!("Failed to resolve path: {}", e)))?,
    );

    // 3. Path sandboxing (web: AllowedRoots; desktop: AllowAll no-op). Must
    //    run before the overlap/DB work below so a forbidden path never
    //    touches the database (matches pre-conversion web ordering).
    policy.check(&new_path)?;

    // 4. Get existing scan roots and check for overlaps
    let existing_roots = crate::db::get_scan_roots(&conn)?;

    for root in existing_roots.iter() {
        let existing_path = normalize_path(
            &Path::new(&root.path)
                .canonicalize()
                .map_err(|e| ApiError::Internal(format!("Failed to resolve existing root path: {}", e)))?,
        );

        // Check exact match
        if new_path == existing_path {
            return Err(ApiError::Conflict("This directory is already being monitored".to_string()));
        }

        // Check if new path is a subdirectory of existing root
        if new_path.starts_with(&existing_path) {
            return Err(ApiError::Conflict(format!(
                "Cannot add directory: it is a subdirectory of existing scan root '{}'",
                root.path
            )));
        }

        // Check if new path is a parent of existing root
        if existing_path.starts_with(&new_path) {
            return Err(ApiError::Conflict(format!(
                "Cannot add directory: existing scan root '{}' is a subdirectory of it",
                root.path
            )));
        }
    }

    // 5. Single-library-root enforcement — checked after overlap validation
    //    so a doomed-anyway overlapping add doesn't get misreported as a
    //    library-uniqueness conflict.
    check_library_root_uniqueness(&conn, &kind)?;

    // 6. Store the canonicalized path
    let path_str = new_path.to_string_lossy().to_string();
    tracing::info!(path = %path_str, kind = %kind, "adding scan root");
    let id = crate::db::upsert_scan_root(&conn, &path_str, &kind).map_err(|e| {
        tracing::error!(path = %path_str, error = %e, "failed to add scan root");
        e
    })?;

    Ok(ScanRoot {
        id: Some(id),
        path: path_str,
        enabled: true,
        find_duplicates: true,
        unique_camera: false,
        last_scan: None,
        last_scan_errors: None,
        monitor_enabled: false,
        kind,
    })
}

/// The (single) calibration library root, if configured. Legacy accessor —
/// kept for the case where the library was created as a dedicated scan root
/// (folder outside every monitored directory). The effective master-write
/// destination is resolved by [`get_calibration_library_dir`], which prefers
/// the `calibration.library_dir` settings key over this root.
pub fn get_calibration_library_root(ctx: &ServiceContext) -> Result<Option<ScanRoot>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_scan_roots(&conn)?
        .into_iter()
        .find(|r| r.kind == "calibration_library"))
}

/// Effective calibration-library directory — where newly built master
/// calibration frames are written (`api::masters`). Two sources, in
/// precedence order:
///
/// 1. The `calibration.library_dir` settings key — set when the operator
///    picks a folder INSIDE an existing monitored directory. No second scan
///    root is created in that case: the parent root already provides scan
///    coverage, and overlapping roots are forbidden because root-scoped
///    maintenance (`delete/recreate_calibration_sets_for_root`,
///    unique-camera reconcile) matches files by path prefix and two roots
///    would fight over the shared subtree.
/// 2. Legacy fallback: the path of the (single) `calibration_library`-kind
///    scan root — created when the picked folder lies outside every
///    monitored directory (so the library still gets scan coverage of its
///    own). Only consulted when the settings key is ABSENT; a
///    present-but-empty key means "explicitly cleared" and blocks the
///    fallback.
pub fn get_calibration_library_dir(ctx: &ServiceContext) -> Result<Option<String>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    resolve_calibration_library_dir(&conn)
}

/// Connection-level resolver behind [`get_calibration_library_dir`]
/// (extracted so precedence is testable without a `ServiceContext`).
pub(crate) fn resolve_calibration_library_dir(
    conn: &rusqlite::Connection,
) -> Result<Option<String>, ApiError> {
    match crate::db::get_setting(conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR)? {
        Some(dir) if !dir.trim().is_empty() => Ok(Some(dir)),
        Some(_) => Ok(None), // present-but-empty: explicitly cleared
        None => Ok(crate::db::get_scan_roots(conn)?
            .into_iter()
            .find(|r| r.kind == "calibration_library")
            .map(|r| r.path)),
    }
}

/// Set the calibration-library directory (master-frame write destination).
///
/// - Folder inside (or equal to) an existing monitored directory → persists
///   the `calibration.library_dir` settings key only. The parent root
///   already provides scan coverage; a nested scan root would violate the
///   no-overlap invariant (see [`get_calibration_library_dir`]).
/// - Folder outside every monitored directory → also adds it as the
///   dedicated `calibration_library`-kind scan root (existing single-library
///   uniqueness/overlap validation applies), so manually dropped masters
///   keep being imported by scans.
///
/// Returns the normalized path that became effective.
pub fn set_calibration_library_dir(
    ctx: &ServiceContext,
    path: String,
    policy: &PathPolicy,
) -> Result<String, ApiError> {
    let path_buf = Path::new(&path);
    if !path_buf.exists() {
        return Err(ApiError::Invalid("Directory does not exist".to_string()));
    }
    let new_path = normalize_path(
        &path_buf
            .canonicalize()
            .map_err(|e| ApiError::Internal(format!("Failed to resolve path: {}", e)))?,
    );
    policy.check(&new_path)?;

    // Covered by an existing root? (`starts_with` is true for equality too.)
    let covered = {
        let db = db(ctx)?;
        let conn = db.conn();
        let existing_roots = crate::db::get_scan_roots(&conn)?;
        let mut covered = false;
        for root in existing_roots.iter() {
            let existing_path = normalize_path(&Path::new(&root.path).canonicalize().map_err(
                |e| ApiError::Internal(format!("Failed to resolve existing root path: {}", e)),
            )?);
            if new_path.starts_with(&existing_path) {
                covered = true;
                break;
            }
        }
        covered
    };

    let path_str = new_path.to_string_lossy().to_string();
    if !covered {
        // Standalone folder — becomes the dedicated library scan root
        // (add_scan_root re-validates overlap + single-library uniqueness).
        add_scan_root(ctx, path_str.clone(), policy, Some("calibration_library".to_string()))?;
    }

    let db = db(ctx)?;
    let conn = db.conn();
    ctx.settings
        .persist_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR, &path_str)?;
    tracing::info!(path = %path_str, covered_by_existing_root = covered, "calibration library dir set");
    Ok(path_str)
}

/// Clear the calibration-library directory setting. Writes an EMPTY value
/// (not a delete) so the legacy `calibration_library`-root fallback stays
/// blocked — see [`get_calibration_library_dir`]. Never deletes any scan
/// root: if the library was a dedicated root it remains a monitored
/// directory, removable through the regular scan-root list (which is also
/// where its catalog-purge consequences are already understood).
pub fn clear_calibration_library_dir(ctx: &ServiceContext) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    ctx.settings
        .persist_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR, "")?;
    tracing::info!("calibration library dir cleared");
    Ok(())
}

pub fn delete_scan_root(ctx: &ServiceContext, id: i64) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::delete_scan_root(&conn, id)?)
}

/// Synchronous, non-progress scan variant. NOTE: the frontend only wires up
/// `start_scan_with_progress` (via `useScanProgress`) — `useScan`/`start_scan`
/// is unreferenced dead code on both transports as of this conversion (see
/// Task 9 report). Kept for parity since it's still registered/reachable.
pub fn start_scan(ctx: &ServiceContext, root_id: i64) -> Result<ScanResultDto, ApiError> {
    let span = tracing::info_span!("scan", root_id);
    let _g = span.enter();

    let db = db(ctx)?;
    let conn = db.conn();

    // Reconcile unique_camera instrume suffix state before scanning
    let reconcile = crate::db::reconcile_unique_camera_instrume(&conn, root_id)
        .map_err(|e| ApiError::Internal(format!("Reconciliation failed: {}", e)))?;

    // Get the scan root path
    let roots = crate::db::get_scan_roots(&conn)?;
    let root = roots
        .into_iter()
        .find(|r| r.id == Some(root_id))
        .ok_or_else(|| ApiError::NotFound("Scan root not found".to_string()))?;

    // Check if content hash should be computed
    let use_content_hash = ctx.settings
        .get_duplicates_use_content_hash(&conn)
        .unwrap_or(false);

    // Perform the scan
    let mut result = crate::scanner::scan_directory(
        Path::new(&root.path), &conn, None, use_content_hash, root.unique_camera, root_id,
    );

    // If reconciliation changed frames, wipe and rebuild calibration sets.
    // Failures propagate: deletions or rebuilds must succeed to maintain
    // data consistency. Matches original desktop behavior (Task 9 drift).
    if reconcile.frames_renamed > 0 {
        crate::db::delete_calibration_sets_for_root(&conn, root_id)?;
        let count = recreate_calibration_sets_for_root(&conn, root_id)?;
        result.calibration_sets_created = count;
    }

    // Update last_scan timestamp
    crate::db::update_scan_root_timestamp(&conn, root_id)?;

    // Persist scan errors so they survive app restarts
    if let Err(e) = crate::db::update_scan_root_errors(&conn, root_id, &result.errors) {
        tracing::error!(root_id, error = %e, "failed to persist scan errors");
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

pub fn rescan_all_for_content_hash(ctx: &ServiceContext) -> Result<RescanResultDto, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    tracing::info!("starting content hash rescan for all files");

    // Get all files from database
    let all_files = crate::db::get_files(&conn, None)?;
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
                            tracing::debug!(current = updated + skipped + missing, total, "content hash rescan progress");
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

    tracing::info!(
        total,
        updated,
        skipped,
        missing,
        errors = errors.len(),
        "content hash rescan complete"
    );

    // Mark content hash rescan as completed
    if updated > 0 || skipped > 0 {
        // Only set flag if we actually processed files successfully
        ctx.settings
            .persist_setting(&conn, "duplicates.content_hash_rescanned", "true")
            .map_err(|e| ApiError::Internal(format!("Failed to set rescan flag: {}", e)))?;
        tracing::debug!("content hash rescan flag set to true");
    }

    Ok(RescanResultDto {
        files_total: total,
        files_updated: updated,
        files_skipped: skipped,
        files_missing: missing,
        errors,
    })
}

/// Relink files from old scan root to new location.
///
/// Path validation/canonicalization/sandboxing (`exists`/`is_dir` checks,
/// `canonicalize`, `policy.check`) is web-only logic pre-conversion (the
/// desktop command trusted the native folder-picker's output verbatim). Per
/// conversion rule 1(a)/(b) it is folded into the single shared body here —
/// desktop now also canonicalizes/validates `new_path` (policy is `AllowAll`
/// there, so the sandboxing check itself is a no-op). See Task 9 report.
pub fn relink_scan_root(
    ctx: &ServiceContext,
    root_id: i64,
    new_path: String,
    policy: &PathPolicy,
) -> Result<RelinkResult, ApiError> {
    let new_path_buf = Path::new(&new_path).to_path_buf();

    if !new_path_buf.exists() {
        return Err(ApiError::Invalid("Directory does not exist".to_string()));
    }
    if !new_path_buf.is_dir() {
        return Err(ApiError::Invalid("Path is not a directory".to_string()));
    }

    let canonical = new_path_buf.canonicalize().map_err(|e| {
        tracing::error!(root_id, path = %new_path, error = %e, "failed to resolve relink target path");
        ApiError::Internal(format!("Failed to resolve path: {}", e))
    })?;

    policy.check(&canonical)?;

    let new_path = canonical.to_string_lossy().to_string();

    let db = db(ctx)?;
    let conn = db.conn();

    // Get old root path
    let old_path: String = conn
        .query_row(
            "SELECT path FROM scan_roots WHERE id = ?1",
            rusqlite::params![root_id],
            |row| row.get(0),
        )
        .map_err(|e| {
            if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                ApiError::NotFound(format!("Scan root {} not found", root_id))
            } else {
                tracing::error!(root_id, path = %new_path, error = %e, "failed to load scan root for relink");
                ApiError::Internal(format!("Failed to get scan root: {}", e))
            }
        })?;

    tracing::info!(root_id, old_path = %old_path, new_path = %new_path, "relinking scan root");

    // Perform relinking
    let result = crate::relinking::relink_files(&conn, &old_path, &new_path)
        .map_err(|e| {
            tracing::error!(root_id, path = %new_path, error = %e, "relinking failed");
            ApiError::Internal(format!("Relinking failed: {}", e))
        })?;

    // Update scan root path if all files were matched
    if result.files_orphaned == 0 || result.files_matched > 0 {
        conn.execute(
            "UPDATE scan_roots SET path = ?1 WHERE id = ?2",
            rusqlite::params![new_path, root_id],
        )
        .map_err(|e| {
            tracing::error!(root_id, path = %new_path, error = %e, "failed to update scan root path");
            ApiError::Internal(format!("Failed to update scan root path: {}", e))
        })?;
        tracing::info!(root_id, new_path = %new_path, "updated scan root path");
    }

    Ok(result)
}

/// Check availability of all scan roots
pub fn check_all_scan_roots_availability(ctx: &ServiceContext) -> Result<Vec<(i64, bool)>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let roots = crate::db::get_scan_roots(&conn)?;

    let availability: Vec<(i64, bool)> = roots
        .into_iter()
        .map(|root| {
            let exists = Path::new(&root.path).exists();
            (root.id.unwrap_or(0), exists)
        })
        .collect();

    Ok(availability)
}

/// Check for missing files within a scan root. Emits `verifying`-phase
/// progress before/after the (parallel) filesystem check.
///
/// Web-side counterpart lives in `routes/missing_files.rs` (not
/// `routes/scan_roots.rs`) and was NOT converted in Task 9 — out of that
/// file's declared scope; see Task 9 report for the gap.
pub fn check_missing_files_in_scan_root<E: crate::events::ProgressEmitter>(
    ctx: &ServiceContext,
    root_id: i64,
    emitter: &E,
) -> Result<Vec<OrphanedFile>, ApiError> {
    use rayon::prelude::*;

    // Emit initial "verifying" phase progress
    crate::scanner::emit_progress(emitter, root_id, 0, 0, None, "verifying");

    // Collect files from database
    let files = {
        let db = db(ctx)?;
        let conn = db.conn();

        // Get scan root path
        let path: String = conn
            .query_row(
                "SELECT path FROM scan_roots WHERE id = ?1",
                rusqlite::params![root_id],
                |row| row.get(0),
            )
            .map_err(|e| ApiError::Internal(format!("Failed to get scan root: {}", e)))?;

        // Get all files under this scan root, excluding files known to live
        // inside an archive zip (`archived_in_operation IS NOT NULL`). Their
        // on-disk paths intentionally don't exist post-archive — flagging
        // them as "missing" would fill the missing_files table with false
        // positives the user has to manually clear.
        // Use LEFT JOIN instead of subqueries to avoid N+1 query problem.
        let mut stmt = conn
            .prepare(
                "SELECT f.id, f.path, f.filename, f.size, f.modified_at,
                        CASE WHEN fr.id IS NOT NULL THEN 1 ELSE 0 END as has_frame,
                        fr.object,
                        fr.date_obs
                 FROM files f
                 LEFT JOIN frames fr ON fr.file_id = f.id
                 WHERE f.path LIKE ?1 AND f.archived_in_operation IS NULL"
            )?;

        let path_prefix = format!("{}%", path);
        let result: Vec<OrphanedFile> = stmt
            .query_map(rusqlite::params![path_prefix], |row| {
                Ok(OrphanedFile {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    filename: row.get(2)?,
                    size: row.get(3)?,
                    modified_at: row.get(4)?,
                    has_frame: row.get::<_, i64>(5)? != 0,
                    object: row.get(6).ok(),
                    date_obs: row.get(7).ok(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        result
    };

    // Filter to only files that don't exist on disk - done in parallel
    // This is done OUTSIDE the lock since filesystem checks can be slow
    let total_files = files.len();

    // Use parallel iteration for filesystem checks (I/O bound operations)
    let missing_files: Vec<OrphanedFile> = files
        .into_par_iter()
        .filter(|file| !Path::new(&file.path).exists())
        .collect();

    // Emit final progress
    crate::scanner::emit_progress(emitter, root_id, total_files, total_files, None, "verifying");

    Ok(missing_files)
}

/// Toggle unique_camera flag (flag-only, cascade happens on re-scan).
///
/// Web-side counterpart lives in `routes/duplicates.rs` (not
/// `routes/scan_roots.rs`) and was NOT converted in Task 9 — out of that
/// file's declared scope; see Task 9 report for the gap.
pub fn set_scan_root_unique_camera_flag(ctx: &ServiceContext, id: i64, enabled: bool) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    crate::db::set_unique_camera_flag(&conn, id, enabled)?;

    tracing::info!(root_id = id, enabled, "unique_camera flag set");

    Ok(())
}

/// Toggle the background-monitoring flag for a scan root. The monitor service
/// polls only roots with this flag set. Persists immediately; the next
/// monitor tick respects the new value.
///
/// `monitor` is passed explicitly because `MonitorService` lives alongside
/// (not inside) `ServiceContext` on both `AppState`/`WebAppState` — see Task 9
/// report for why this handler's signature extends the base recipe shape.
pub fn set_scan_root_monitor_enabled(
    ctx: &ServiceContext,
    id: i64,
    enabled: bool,
    monitor: &MonitorService,
) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    crate::db::set_scan_root_monitor_enabled(&conn, id, enabled)?;

    tracing::info!(root_id = id, monitor_enabled = enabled, "scan root monitor toggled");

    // Wake the monitor loop so the user gets an immediate scan instead of
    // waiting for the current sleep to finish. Only relevant when enabling.
    if enabled {
        monitor.kick();
    }

    Ok(())
}

/// Query calibration frame IDs under a scan root and recreate calibration
/// sets. Single copy — was copy-pasted across `commands/scan_roots.rs` and
/// `routes/scan_roots.rs` pre-conversion; merged here now per Task 9 BINDING
/// detail #4 (originally scheduled for Task 12 Step 4).
fn recreate_calibration_sets_for_root(
    conn: &rusqlite::Connection,
    root_id: i64,
) -> Result<usize, ApiError> {
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

    tracing::debug!(
        root_id,
        flats = flat_ids.len(),
        darks = dark_ids.len(),
        bias = bias_ids.len(),
        darkflats = darkflat_ids.len(),
        masters = master_ids.total_count(),
        "recreating calibration sets for root"
    );

    let scan_result = create_calibration_sets_from_scan_with_masters(
        conn, flat_ids, dark_ids, bias_ids, darkflat_ids, master_ids,
    )?;

    Ok(scan_result.sets_created as usize)
}

/// Start a scan with progress events. Runs the shared scan engine
/// (`scanner::run_registered_scan`, which registers the scan handle,
/// reconciles, scans, and persists), then — best-effort — rebuilds
/// calibration sets if reconciliation renamed frames.
///
/// `run_registered_scan`'s `Err(String)` classification (web-only logic
/// pre-conversion) is folded in here per rule 1(b): "already in progress" ->
/// `Conflict`, "not found" -> `NotFound`, everything else -> `Internal`.
pub fn start_scan_with_progress<E: crate::events::ProgressEmitter>(
    ctx: &ServiceContext,
    root_id: i64,
    emitter: &E,
) -> Result<ScanResultDto, ApiError> {
    tracing::info!(root_id = root_id, "scan started");

    let outcome = crate::scanner::run_registered_scan(ctx, emitter, root_id).map_err(|e| {
        if e.contains("already in progress") {
            ApiError::Conflict(e)
        } else if e.contains("not found") {
            ApiError::NotFound(e)
        } else {
            ApiError::Internal(e)
        }
    })?;
    let mut result = outcome.result;
    let reconcile = outcome.reconcile;

    // Interactive-only follow-up: if reconciliation renamed frames, rebuild
    // calibration sets under this root. Monitor cycles never trigger this path
    // because they don't toggle `unique_camera`. Failures propagate to maintain
    // data consistency. Matches original desktop behavior (Task 9 drift).
    if reconcile.frames_renamed > 0 {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::delete_calibration_sets_for_root(&conn, root_id)?;
        let count = recreate_calibration_sets_for_root(&conn, root_id)?;
        result.calibration_sets_created = count;
    }

    tracing::info!(
        root_id = root_id,
        found = result.files_found,
        processed = result.files_processed,
        skipped = result.files_skipped,
        errors = result.errors.len(),
        "scan complete"
    );

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
pub fn cancel_scan(ctx: &ServiceContext, root_id: i64) -> Result<(), ApiError> {
    let scans = ctx.active_scans.lock().unwrap();
    if let Some(handle) = scans.get(&root_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        Ok(())
    } else {
        Err(ApiError::NotFound("No active scan for this root".to_string()))
    }
}

/// Web-only: no Tauri command by this name (desktop has no analogous
/// command). Converted anyway for single-sourcing since it lives in
/// `routes/scan_roots.rs`, in this task's declared file scope. Returns a map
/// of scan_root_id -> count of files with 'missing' status.
pub fn get_missing_files_counts(ctx: &ServiceContext) -> Result<HashMap<i64, i64>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let mut stmt = conn.prepare(
        "SELECT scan_root_id, COUNT(*)
         FROM missing_files
         WHERE status = 'missing'
         GROUP BY scan_root_id",
    )?;

    let mut counts = HashMap::new();
    let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;

    for row in rows {
        let (root_id, count) = row?;
        counts.insert(root_id, count);
    }

    Ok(counts)
}

/// Task 9 fix round — api-level coverage for `add_scan_root`'s kind checks.
/// The two checks run at different points in `add_scan_root`'s flow (Invalid
/// before any path/DB work; Conflict after overlap validation), so they were
/// extracted as two conn-level functions and are pinned here directly —
/// same pattern as Task 5's fix round (test the extracted real logic, not a
/// local re-implementation).
#[cfg(test)]
mod kind_check_tests {
    use super::*;

    fn test_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn bogus_kind_is_invalid() {
        assert!(matches!(
            validate_scan_root_kind("master_stash"),
            Err(ApiError::Invalid(_))
        ));
    }

    #[test]
    fn known_kinds_pass_validation() {
        assert!(validate_scan_root_kind("normal").is_ok());
        assert!(validate_scan_root_kind("calibration_library").is_ok());
    }

    // ── resolve_calibration_library_dir precedence ───────────────────────

    #[test]
    fn library_dir_key_wins_over_library_root() {
        let conn = test_conn();
        crate::db::upsert_scan_root(&conn, "/lib/a", "calibration_library").unwrap();
        crate::db::set_setting(
            &conn,
            crate::settings::keys::CALIBRATION_LIBRARY_DIR,
            "/data/masters",
        )
        .unwrap();
        assert_eq!(
            resolve_calibration_library_dir(&conn).unwrap(),
            Some("/data/masters".to_string())
        );
    }

    #[test]
    fn library_dir_falls_back_to_library_root_when_key_absent() {
        let conn = test_conn();
        crate::db::upsert_scan_root(&conn, "/lib/a", "calibration_library").unwrap();
        crate::db::upsert_scan_root(&conn, "/data/normal", "normal").unwrap();
        assert_eq!(
            resolve_calibration_library_dir(&conn).unwrap(),
            Some("/lib/a".to_string())
        );
    }

    #[test]
    fn empty_library_dir_key_blocks_root_fallback() {
        // Present-but-empty = explicitly cleared: the legacy root must NOT
        // resurface, otherwise "Remove" in the UI would appear to do nothing.
        let conn = test_conn();
        crate::db::upsert_scan_root(&conn, "/lib/a", "calibration_library").unwrap();
        crate::db::set_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR, "").unwrap();
        assert_eq!(resolve_calibration_library_dir(&conn).unwrap(), None);
    }

    #[test]
    fn library_dir_none_when_unconfigured() {
        let conn = test_conn();
        crate::db::upsert_scan_root(&conn, "/data/normal", "normal").unwrap();
        assert_eq!(resolve_calibration_library_dir(&conn).unwrap(), None);
    }

    #[test]
    fn second_library_root_is_conflict() {
        let conn = test_conn();
        crate::db::upsert_scan_root(&conn, "/lib/a", "calibration_library").unwrap();
        assert!(matches!(
            check_library_root_uniqueness(&conn, "calibration_library"),
            Err(ApiError::Conflict(_))
        ));
    }

    #[test]
    fn normal_kind_is_ok_despite_existing_library_root() {
        let conn = test_conn();
        crate::db::upsert_scan_root(&conn, "/lib/a", "calibration_library").unwrap();
        assert!(check_library_root_uniqueness(&conn, "normal").is_ok());
    }

    #[test]
    fn first_library_root_is_ok() {
        let conn = test_conn();
        assert!(check_library_root_uniqueness(&conn, "calibration_library").is_ok());
    }
}
