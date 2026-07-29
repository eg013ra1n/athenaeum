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
    /// File ids freshly ingested by this scan — consumed by the command layer to
    /// drive the personal-sync auto-mode enqueue (task M2). Empty for the
    /// non-progress `start_scan` path.
    pub new_file_ids: Vec<i64>,
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

/// Scan-root kinds that are enforced unique across all roots and are managed
/// through dedicated designate/clear commands rather than the plain
/// add/delete flow. Each is guarded against plain deletion (see
/// [`guard_against_special_root_deletion`]).
pub(crate) const SPECIAL_ROOT_KINDS: &[&str] =
    &["calibration_library", "sync_incoming", "collaboration"];

/// Human-facing label for a special scan-root kind, used in the uniqueness /
/// deletion-guard error messages. The `"Calibration Library"` wording is
/// load-bearing: `set_calibration_library_dir` string-matches on it — do not
/// change it.
fn special_root_label(kind: &str) -> &'static str {
    match kind {
        "calibration_library" => "Calibration Library",
        "sync_incoming" => "Sync incoming folder",
        "collaboration" => "Collaboration folder",
        _ => "special",
    }
}

/// Validate a scan-root `kind` value: `"normal"` or one of
/// [`SPECIAL_ROOT_KINDS`]; anything else is `ApiError::Invalid`. Runs at the
/// top of `add_scan_root`, before any path/DB work.
pub(crate) fn validate_scan_root_kind(kind: &str) -> Result<(), ApiError> {
    if kind != "normal" && !SPECIAL_ROOT_KINDS.contains(&kind) {
        return Err(ApiError::Invalid(format!("unknown scan root kind: {kind}")));
    }
    Ok(())
}

/// Single-special-root enforcement: adding a root whose `kind` is one of
/// [`SPECIAL_ROOT_KINDS`] when one already exists is `ApiError::Conflict`.
/// A no-op for `"normal"`. Runs in `add_scan_root` after overlap validation,
/// before the DB write.
pub(crate) fn check_special_root_uniqueness(
    conn: &rusqlite::Connection,
    kind: &str,
) -> Result<(), ApiError> {
    if SPECIAL_ROOT_KINDS.contains(&kind) && crate::db::count_scan_roots_of_kind(conn, kind)? > 0 {
        return Err(ApiError::Conflict(format!(
            "A {} root already exists — only one is allowed",
            special_root_label(kind)
        )));
    }
    Ok(())
}

/// Dry-run verdict for an Add Folder candidate (teaching dialog, spec §6).
/// `ok == false` carries a machine-readable `reason` the dialog maps to copy.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
pub struct FolderCandidateVerdict {
    pub ok: bool,
    /// `not_found` | `not_a_directory` | `already_monitored` |
    /// `inside_existing` | `contains_existing` | `role_taken`
    pub reason: Option<String>,
    /// Conflicting monitored path (`inside_existing`/`contains_existing`) or
    /// the current role path (`role_taken`).
    pub conflicting_path: Option<String>,
    /// Calibration-library only: `covered` (stored as a setting; the parent
    /// root provides scan coverage) or `standalone` (becomes its own root).
    pub placement: Option<String>,
}

fn verdict_fail(reason: &str, conflicting: Option<String>) -> FolderCandidateVerdict {
    FolderCandidateVerdict {
        ok: false,
        reason: Some(reason.to_string()),
        conflicting_path: conflicting,
        placement: None,
    }
}

fn verdict_ok(placement: Option<&str>) -> FolderCandidateVerdict {
    FolderCandidateVerdict {
        ok: true,
        reason: None,
        conflicting_path: None,
        placement: placement.map(str::to_string),
    }
}

/// Connection-level classifier behind [`validate_folder_candidate`] —
/// mirrors `add_scan_root`'s overlap/uniqueness checks (and
/// `set_calibration_library_dir`'s covered-placement rule) WITHOUT writing.
/// `candidate` must already be canonicalized. `kind == "archive"` skips
/// placement checks entirely (archive destinations are never scanned).
pub(crate) fn classify_folder_candidate(
    conn: &rusqlite::Connection,
    kind: &str,
    candidate: &Path,
) -> Result<FolderCandidateVerdict, ApiError> {
    if kind == "archive" {
        return Ok(verdict_ok(None));
    }
    validate_scan_root_kind(kind)?;

    // Role already assigned? (calibration resolves settings-key-aware)
    if kind == "calibration_library" {
        if let Some(dir) = resolve_calibration_library_dir(conn)? {
            return Ok(verdict_fail("role_taken", Some(dir)));
        }
    } else if SPECIAL_ROOT_KINDS.contains(&kind) {
        if let Some(dir) = resolve_special_root_dir(conn, kind)? {
            return Ok(verdict_fail("role_taken", Some(dir)));
        }
    }

    let is_calibration = kind == "calibration_library";
    for root in crate::db::get_scan_roots(conn)?.iter() {
        let existing = canonical_or_raw(&root.path);
        if candidate == existing {
            return Ok(if is_calibration {
                verdict_ok(Some("covered"))
            } else {
                verdict_fail("already_monitored", Some(root.path.clone()))
            });
        }
        if candidate.starts_with(&existing) {
            return Ok(if is_calibration {
                verdict_ok(Some("covered"))
            } else {
                verdict_fail("inside_existing", Some(root.path.clone()))
            });
        }
        if existing.starts_with(candidate) {
            return Ok(verdict_fail("contains_existing", Some(root.path.clone())));
        }
    }
    Ok(verdict_ok(is_calibration.then_some("standalone")))
}

/// Dry-run validation for the Add Folder dialog. Never writes; the actual
/// add/set command remains authoritative (a TOCTOU between validate and add
/// is acceptable — the add's own error still surfaces).
pub fn validate_folder_candidate(
    ctx: &ServiceContext,
    kind: String,
    path: String,
    policy: &PathPolicy,
) -> Result<FolderCandidateVerdict, ApiError> {
    let path_buf = Path::new(&path);
    if !path_buf.exists() {
        return Ok(verdict_fail("not_found", None));
    }
    if !path_buf.is_dir() {
        return Ok(verdict_fail("not_a_directory", None));
    }
    let canon = normalize_path(
        &path_buf
            .canonicalize()
            .map_err(|e| ApiError::Internal(format!("Failed to resolve path: {}", e)))?,
    );
    policy.check(&canon)?;
    let db = db(ctx)?;
    let conn = db.conn();
    classify_folder_candidate(&conn, &kind, &canon)
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
/// `kind` defaults to `"normal"` when `None`. Each of [`SPECIAL_ROOT_KINDS`]
/// (`"calibration_library"`, `"sync_incoming"`, `"collaboration"`) is enforced
/// unique across all scan roots (`ApiError::Conflict` if a root of that kind
/// already exists); any other value is `ApiError::Invalid`.
/// Note: the overlap checks below reject a new root that is a subdirectory
/// of an existing one — so a special-kind root nested inside an existing scan
/// root is rejected too. That's intentional: a special root is itself just a
/// normal scanned root, so it must live outside every other registered root
/// rather than inside one.
///
/// The kind checks themselves live in `validate_scan_root_kind` /
/// `check_special_root_uniqueness` (extracted so they're testable at the
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

    // 5. Single-special-root enforcement — checked after overlap validation
    //    so a doomed-anyway overlapping add doesn't get misreported as a
    //    uniqueness conflict.
    check_special_root_uniqueness(&conn, &kind)?;

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

/// Validates a candidate calibration-library path: must exist and be a
/// directory. Mirrors `add_scan_root`'s exists/is_dir pair (same messages)
/// — kept as its own function rather than shared so each call site's error
/// text stays independently editable, and so this one is directly testable
/// without a `ServiceContext` (Important-3).
pub(crate) fn validate_library_dir_candidate(path_buf: &Path) -> Result<(), ApiError> {
    if !path_buf.exists() {
        return Err(ApiError::Invalid("Directory does not exist".to_string()));
    }
    if !path_buf.is_dir() {
        return Err(ApiError::Invalid("Path is not a directory".to_string()));
    }
    Ok(())
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
    validate_library_dir_candidate(path_buf)?;
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
        add_scan_root(ctx, path_str.clone(), policy, Some("calibration_library".to_string()))
            .map_err(|e| match e {
                // Minor-4/5: switching from one standalone calibration
                // folder to another hits `check_special_root_uniqueness`'s
                // bare "only one is allowed" — a dead end unless the
                // operator already knows the fix. Spell it out here rather
                // than leaving them to reverse-engineer it. Overlap
                // conflicts (subdirectory/parent/duplicate) pass through
                // unchanged — those are unambiguous already.
                ApiError::Conflict(msg) if msg.starts_with("A Calibration Library root already exists") => {
                    // Order matters: the deletion guard blocks removing the old
                    // root while the setting still resolves to it, so Clear
                    // must come first. And be honest about the purge: removing
                    // the root deletes its masters' CATALOG rows (files on
                    // disk survive; a rescan of a covering root re-imports).
                    ApiError::Conflict(format!(
                        "{msg}. Clear the Calibration Folder first, then remove the old dedicated library root under Monitored Directories (master files on disk are kept; their catalog entries are removed with the root)."
                    ))
                }
                other => other,
            })?;
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

/// One-step calibration-library move (spec §8.1): removes the old dedicated
/// `calibration_library` root (catalog purge — same semantics as deleting it
/// from the folder list; files on disk untouched), then delegates to
/// [`set_calibration_library_dir`] for the covered/standalone placement of
/// the new folder. Replaces the old clear → remove-root → set dance.
///
/// Deliberately bypasses [`guard_against_special_root_deletion`]: the guard
/// exists to stop an operator removing the library out from under the role;
/// here removing it IS the requested operation. Not atomic across the two
/// phases — if the final set fails, the old root is already gone and no
/// library is configured; the UI confirmation warns about the removal, and
/// the error from the set phase surfaces verbatim.
///
/// Re-picking the folder that is already the library is a no-op keep: the old
/// root is left alone (it equals the new path), and the delegate's
/// covered-placement branch simply re-persists the settings key — so the
/// single-library uniqueness check is never reached.
pub fn switch_calibration_library_dir(
    ctx: &ServiceContext,
    path: String,
    policy: &PathPolicy,
) -> Result<String, ApiError> {
    let path_buf = Path::new(&path);
    validate_library_dir_candidate(path_buf)?;
    let new_path = normalize_path(
        &path_buf
            .canonicalize()
            .map_err(|e| ApiError::Internal(format!("Failed to resolve path: {}", e)))?,
    );
    policy.check(&new_path)?;

    let old_root = get_calibration_library_root(ctx)?;
    if let Some(old) = old_root {
        if canonical_or_raw(&old.path) != new_path {
            let id = old.id.ok_or_else(|| {
                ApiError::Internal("calibration library root has no id".to_string())
            })?;
            tracing::info!(src = %old.path, dest = %new_path.display(), "switching calibration library — removing old dedicated root");
            let db = db(ctx)?;
            let conn = db.conn();
            crate::db::delete_scan_root(&conn, id).map_err(|e| {
                tracing::error!(root_id = id, error = %e, "failed to remove old calibration library root");
                ApiError::Internal(format!("Failed to remove old calibration library root: {e}"))
            })?;
        }
    }

    set_calibration_library_dir(ctx, new_path.to_string_lossy().to_string(), policy)
}

// ── Sync-incoming / collaboration special roots (Task 4) ────────────────────
//
// Same designate/get/clear shape as the calibration library, but a simpler
// storage model: the folder IS its own dedicated scan root (there is no
// settings-key precedence layer). One root per kind, enforced by
// `check_special_root_uniqueness` inside `add_scan_root`; guarded against
// plain deletion by `guard_against_special_root_deletion`.

/// Path of the (single) scan root of `kind`, if configured. Shared by the
/// `sync_incoming` / `collaboration` getters and the deletion guard.
fn resolve_special_root_dir(
    conn: &rusqlite::Connection,
    kind: &str,
) -> Result<Option<String>, ApiError> {
    Ok(crate::db::get_scan_roots(conn)?
        .into_iter()
        .find(|r| r.kind == kind)
        .map(|r| r.path))
}

fn get_special_root_dir(ctx: &ServiceContext, kind: &str) -> Result<Option<String>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    resolve_special_root_dir(&conn, kind)
}

/// Designate `path` as the (single) root of `kind`. Routes through
/// `add_scan_root` — which validates existence/dir, canonicalizes, sandboxes
/// against `policy`, rejects overlap with existing roots, and enforces
/// single-`kind` uniqueness (`ApiError::Conflict`) — exactly like the
/// calibration setter's standalone-folder branch. Returns the normalized path.
fn set_special_root_dir(
    ctx: &ServiceContext,
    path: String,
    policy: &PathPolicy,
    kind: &str,
) -> Result<String, ApiError> {
    let root = add_scan_root(ctx, path, policy, Some(kind.to_string()))?;
    tracing::info!(path = %root.path, kind, "special scan root designated");
    Ok(root.path)
}

/// Clear the (single) root of `kind` by DEMOTING it back to a `"normal"`
/// monitored directory — never deletes the row. Mirrors
/// `clear_calibration_library_dir`'s "never deletes any scan root": the folder
/// stays monitored and is removable through the regular scan-root list, while
/// the getter now returns `None` (no root of that kind resolves) and the
/// deletion guard no longer blocks it.
fn clear_special_root_dir(ctx: &ServiceContext, kind: &str) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    conn.execute(
        "UPDATE scan_roots SET kind = 'normal' WHERE kind = ?1",
        rusqlite::params![kind],
    )?;
    tracing::info!(kind, "special scan root cleared");
    Ok(())
}

/// Sync-incoming folder — where the personal-sync receiver writes files
/// pulled from a paired capture device (consumed by Task 5). `None` when
/// unconfigured.
pub fn get_sync_incoming_dir(ctx: &ServiceContext) -> Result<Option<String>, ApiError> {
    get_special_root_dir(ctx, "sync_incoming")
}

/// Designate the sync-incoming folder. See [`set_special_root_dir`].
pub fn set_sync_incoming_dir(
    ctx: &ServiceContext,
    path: String,
    policy: &PathPolicy,
) -> Result<String, ApiError> {
    set_special_root_dir(ctx, path, policy, "sync_incoming")
}

/// Clear the sync-incoming folder (demotes to a normal monitored directory).
pub fn clear_sync_incoming_dir(ctx: &ServiceContext) -> Result<(), ApiError> {
    clear_special_root_dir(ctx, "sync_incoming")
}

/// Collaboration folder — the shared drop location for a collaboration
/// workflow. `None` when unconfigured.
pub fn get_collaboration_dir(ctx: &ServiceContext) -> Result<Option<String>, ApiError> {
    get_special_root_dir(ctx, "collaboration")
}

/// Designate the collaboration folder. See [`set_special_root_dir`].
pub fn set_collaboration_dir(
    ctx: &ServiceContext,
    path: String,
    policy: &PathPolicy,
) -> Result<String, ApiError> {
    set_special_root_dir(ctx, path, policy, "collaboration")
}

/// Clear the collaboration folder (demotes to a normal monitored directory).
pub fn clear_collaboration_dir(ctx: &ServiceContext) -> Result<(), ApiError> {
    clear_special_root_dir(ctx, "collaboration")
}

/// Best-effort canonicalize for path comparison: falls back to the raw path
/// when the target no longer exists on disk (a stale registered path, or a
/// calibration folder the operator already deleted outside the app — see
/// Important-2's `check_library_dir_exists`). Paths stored in `scan_roots`
/// and the `calibration.library_dir` setting were canonicalized at write
/// time, so the raw-string fallback still compares correctly even when a
/// fresh `canonicalize()` call can't run.
fn canonical_or_raw(path: &str) -> std::path::PathBuf {
    let p = Path::new(path);
    normalize_path(&p.canonicalize().unwrap_or_else(|_| p.to_path_buf()))
}

/// Guard behind `delete_scan_root`'s Critical-1 fix: refuses to delete a
/// root whose subtree contains (or IS) a currently active special-kind
/// directory ([`SPECIAL_ROOT_KINDS`]). `db::delete_scan_root` purges files,
/// frames, etc. by path PREFIX with no awareness of these special roots, so an
/// unguarded delete would silently wipe a nested calibration folder's
/// registered masters (or a dedicated special root's own contents) with no
/// way back.
///
/// - **Calibration library**: resolved via the settings-key-aware
///   [`resolve_calibration_library_dir`] (covers the nested-folder case AND
///   the dedicated-root case). A *vestigial* `calibration_library`-kind root —
///   one the `calibration.library_dir` setting no longer points at — is
///   intentionally NOT blocked: deleting it is the documented cleanup path
///   (Minor-5).
/// - **Sync-incoming / collaboration**: dedicated roots resolved directly by
///   kind. Blocked so they can only be removed through their own `clear_*`
///   command, which demotes them back to a normal monitored directory (after
///   which this guard no longer blocks them).
///
/// Extracted to `Connection` level (no `ServiceContext`) so it's directly
/// testable with an in-memory DB + real temp dirs — same pattern as
/// `check_special_root_uniqueness` / `resolve_calibration_library_dir` above.
pub(crate) fn guard_against_special_root_deletion(
    conn: &rusqlite::Connection,
    root_path: &str,
) -> Result<(), ApiError> {
    let root_canon = canonical_or_raw(root_path);

    // Calibration library — settings-key-aware resolver. `starts_with` covers
    // both the nested-folder case and the dedicated-root case (a path
    // starts_with itself).
    if let Some(lib_dir) = resolve_calibration_library_dir(conn)? {
        if canonical_or_raw(&lib_dir).starts_with(&root_canon) {
            return Err(ApiError::Conflict(format!(
                "This directory contains your Calibration Folder ({lib_dir}). Clear or move the Calibration Folder in File Manager first, then remove the directory."
            )));
        }
    }

    // Sync-incoming / collaboration — dedicated roots resolved directly by
    // kind. (Calibration is handled above via its settings-key resolver, so it
    // is not repeated here.)
    for kind in ["sync_incoming", "collaboration"] {
        if let Some(dir) = resolve_special_root_dir(conn, kind)? {
            if canonical_or_raw(&dir).starts_with(&root_canon) {
                let label = special_root_label(kind);
                return Err(ApiError::Conflict(format!(
                    "This directory is your {label} ({dir}). Clear the {label} in File Manager first, then remove the directory."
                )));
            }
        }
    }
    Ok(())
}

pub fn delete_scan_root(ctx: &ServiceContext, id: i64) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let root_path: String = conn
        .query_row(
            "SELECT path FROM scan_roots WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .map_err(|e| {
            if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                ApiError::NotFound(format!("Scan root {} not found", id))
            } else {
                ApiError::Internal(format!("Failed to get scan root: {}", e))
            }
        })?;
    guard_against_special_root_deletion(&conn, &root_path)?;

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

    // Task E: the scanner ALWAYS computes content_hash so the whole scanned
    // library feeds the device-to-device transfer dedup index (sampling hash vs
    // `files.content_hash`), not just sync-ingested files. The
    // `duplicates.use_content_hash` setting now governs ONLY the Duplicates-view
    // grouping (`find_duplicate_groups`), never whether the scanner hashes.
    let mut result = crate::scanner::scan_directory(
        Path::new(&root.path), &conn, None, true, root.unique_camera, root_id,
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
        new_file_ids: result.new_file_ids,
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
        new_file_ids: result.new_file_ids,
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
        assert!(validate_scan_root_kind("sync_incoming").is_ok());
        assert!(validate_scan_root_kind("collaboration").is_ok());
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
            check_special_root_uniqueness(&conn, "calibration_library"),
            Err(ApiError::Conflict(_))
        ));
    }

    #[test]
    fn normal_kind_is_ok_despite_existing_library_root() {
        let conn = test_conn();
        crate::db::upsert_scan_root(&conn, "/lib/a", "calibration_library").unwrap();
        assert!(check_special_root_uniqueness(&conn, "normal").is_ok());
    }

    #[test]
    fn first_library_root_is_ok() {
        let conn = test_conn();
        assert!(check_special_root_uniqueness(&conn, "calibration_library").is_ok());
    }
}

/// Critical-1 fix round — `guard_against_special_root_deletion`
/// exercised at `Connection` level with real temp dirs (canonicalize needs
/// real paths on disk), matching the "test the extracted real logic"
/// convention established in `kind_check_tests` above. Covers the five
/// scenarios from the fix-round brief: nested folder blocks, unrelated root
/// is fine, active dedicated root blocks, vestigial dedicated root is fine,
/// and clearing the setting unblocks a previously-blocked deletion.
#[cfg(test)]
mod delete_guard_tests {
    use super::*;
    use tempfile::TempDir;

    fn test_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn nested_calibration_folder_blocks_root_deletion() {
        let root = TempDir::new().unwrap();
        let calib = root.path().join("calib");
        std::fs::create_dir_all(&calib).unwrap();

        let conn = test_conn();
        let root_path = root.path().canonicalize().unwrap().to_string_lossy().to_string();
        crate::db::upsert_scan_root(&conn, &root_path, "normal").unwrap();
        crate::db::set_setting(
            &conn,
            crate::settings::keys::CALIBRATION_LIBRARY_DIR,
            &calib.canonicalize().unwrap().to_string_lossy(),
        )
        .unwrap();

        assert!(matches!(
            guard_against_special_root_deletion(&conn, &root_path),
            Err(ApiError::Conflict(_))
        ));
    }

    #[test]
    fn unrelated_root_deletes_fine() {
        let root_a = TempDir::new().unwrap();
        let lib = TempDir::new().unwrap();

        let conn = test_conn();
        let root_a_path = root_a.path().canonicalize().unwrap().to_string_lossy().to_string();
        crate::db::upsert_scan_root(&conn, &root_a_path, "normal").unwrap();
        crate::db::set_setting(
            &conn,
            crate::settings::keys::CALIBRATION_LIBRARY_DIR,
            &lib.path().canonicalize().unwrap().to_string_lossy(),
        )
        .unwrap();

        assert!(guard_against_special_root_deletion(&conn, &root_a_path).is_ok());
    }

    #[test]
    fn active_dedicated_root_blocks() {
        let lib_root = TempDir::new().unwrap();

        let conn = test_conn();
        let lib_root_path = lib_root.path().canonicalize().unwrap().to_string_lossy().to_string();
        // No `calibration.library_dir` setting — resolve falls back to the
        // legacy `calibration_library`-kind root, which IS the one being
        // deleted here.
        crate::db::upsert_scan_root(&conn, &lib_root_path, "calibration_library").unwrap();

        assert!(matches!(
            guard_against_special_root_deletion(&conn, &lib_root_path),
            Err(ApiError::Conflict(_))
        ));
    }

    #[test]
    fn vestigial_calibration_library_root_deletes_fine() {
        let old_lib_root = TempDir::new().unwrap();
        let new_lib = TempDir::new().unwrap();

        let conn = test_conn();
        let old_lib_root_path = old_lib_root.path().canonicalize().unwrap().to_string_lossy().to_string();
        // Vestigial: still registered as a calibration_library-kind root,
        // but the settings key has since moved on to somewhere else.
        crate::db::upsert_scan_root(&conn, &old_lib_root_path, "calibration_library").unwrap();
        crate::db::set_setting(
            &conn,
            crate::settings::keys::CALIBRATION_LIBRARY_DIR,
            &new_lib.path().canonicalize().unwrap().to_string_lossy(),
        )
        .unwrap();

        assert!(guard_against_special_root_deletion(&conn, &old_lib_root_path).is_ok());
    }

    #[test]
    fn after_clear_the_blocked_deletion_succeeds() {
        let root = TempDir::new().unwrap();
        let calib = root.path().join("calib");
        std::fs::create_dir_all(&calib).unwrap();

        let conn = test_conn();
        let root_path = root.path().canonicalize().unwrap().to_string_lossy().to_string();
        crate::db::upsert_scan_root(&conn, &root_path, "normal").unwrap();
        crate::db::set_setting(
            &conn,
            crate::settings::keys::CALIBRATION_LIBRARY_DIR,
            &calib.canonicalize().unwrap().to_string_lossy(),
        )
        .unwrap();

        // Blocked before clearing...
        assert!(guard_against_special_root_deletion(&conn, &root_path).is_err());

        // clear_calibration_library_dir writes an empty value (not a
        // delete) — mirror that here rather than re-implementing via ctx.
        crate::db::set_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR, "").unwrap();

        // ...and allowed after.
        assert!(guard_against_special_root_deletion(&conn, &root_path).is_ok());
    }
}

/// Important-3 fix round — `set_calibration_library_dir` rejects a file path
/// the same way `add_scan_root` does, instead of silently accepting it (only
/// to fail later when a master build tries to write into "the folder").
/// Exercises `validate_library_dir_candidate` directly — the function
/// `set_calibration_library_dir` calls first, before any DB/settings work —
/// since the full function needs a `ServiceContext` that none of this
/// module's tests construct (see `kind_check_tests`' doc comment for the
/// established "test the extracted real logic, not a local
/// re-implementation" convention this follows).
#[cfg(test)]
mod set_library_dir_validation_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn file_path_inside_a_root_is_invalid() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("not_a_dir.txt");
        std::fs::write(&file_path, b"hello").unwrap();

        assert!(matches!(
            validate_library_dir_candidate(&file_path),
            Err(ApiError::Invalid(_))
        ));
    }

    #[test]
    fn real_directory_passes() {
        let dir = TempDir::new().unwrap();
        assert!(validate_library_dir_candidate(dir.path()).is_ok());
    }

    #[test]
    fn nonexistent_path_is_invalid() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("does_not_exist");
        assert!(matches!(
            validate_library_dir_candidate(&missing),
            Err(ApiError::Invalid(_))
        ));
    }
}

/// Task 4 (Stage 1.5 sync-hardening) — the `sync_incoming` / `collaboration`
/// special scan-root kinds. Cloned from the calibration-library semantics:
/// one root per kind, designated/cleared through dedicated commands, guarded
/// against plain deletion. Exercised at the `ServiceContext` level (real temp
/// catalog + real temp folders) because the setters route through
/// `add_scan_root`, which stat-checks and canonicalizes the folder on disk.
#[cfg(test)]
mod special_root_tests {
    use super::*;
    use crate::services::ServiceContext;
    use tempfile::TempDir;

    fn test_ctx(db_dir: &TempDir) -> ServiceContext {
        ServiceContext::new_for_tests(db_dir.path().join("catalog.db"))
    }

    fn kind_of(ctx: &ServiceContext, path: &str) -> Option<String> {
        let db = db(ctx).unwrap();
        let conn = db.conn();
        conn.query_row(
            "SELECT kind FROM scan_roots WHERE path = ?1",
            rusqlite::params![path],
            |r| r.get(0),
        )
        .ok()
    }

    #[test]
    fn sync_incoming_root_set_get_clear_roundtrip() {
        let db_dir = TempDir::new().unwrap();
        let ctx = test_ctx(&db_dir);
        let folder = TempDir::new().unwrap();
        let folder2 = TempDir::new().unwrap();

        // set → get returns the (canonicalized) path, row is kind='sync_incoming'
        let stored = set_sync_incoming_dir(
            &ctx,
            folder.path().to_string_lossy().to_string(),
            &PathPolicy::AllowAll,
        )
        .unwrap();
        assert_eq!(get_sync_incoming_dir(&ctx).unwrap(), Some(stored.clone()));
        assert_eq!(kind_of(&ctx, &stored).as_deref(), Some("sync_incoming"));

        // second set of a DIFFERENT path → Conflict (per-kind uniqueness)
        assert!(matches!(
            set_sync_incoming_dir(
                &ctx,
                folder2.path().to_string_lossy().to_string(),
                &PathPolicy::AllowAll,
            ),
            Err(ApiError::Conflict(_))
        ));

        // clear → get returns None; row demoted to 'normal' (never deleted),
        // mirroring clear_calibration_library_dir's "never deletes any root".
        clear_sync_incoming_dir(&ctx).unwrap();
        assert_eq!(get_sync_incoming_dir(&ctx).unwrap(), None);
        assert_eq!(kind_of(&ctx, &stored).as_deref(), Some("normal"));
    }

    #[test]
    fn collaboration_root_uniqueness_independent_of_sync_incoming() {
        let db_dir = TempDir::new().unwrap();
        let ctx = test_ctx(&db_dir);
        let sync_dir = TempDir::new().unwrap();
        let collab_dir = TempDir::new().unwrap();
        let collab_dir2 = TempDir::new().unwrap();

        // one sync_incoming AND one collaboration root coexist
        set_sync_incoming_dir(
            &ctx,
            sync_dir.path().to_string_lossy().to_string(),
            &PathPolicy::AllowAll,
        )
        .unwrap();
        set_collaboration_dir(
            &ctx,
            collab_dir.path().to_string_lossy().to_string(),
            &PathPolicy::AllowAll,
        )
        .unwrap();
        assert!(get_sync_incoming_dir(&ctx).unwrap().is_some());
        assert!(get_collaboration_dir(&ctx).unwrap().is_some());

        // a SECOND collaboration root conflicts (uniqueness is per-kind)
        assert!(matches!(
            set_collaboration_dir(
                &ctx,
                collab_dir2.path().to_string_lossy().to_string(),
                &PathPolicy::AllowAll,
            ),
            Err(ApiError::Conflict(_))
        ));
    }

    #[test]
    fn special_roots_reject_plain_delete() {
        let db_dir = TempDir::new().unwrap();
        let ctx = test_ctx(&db_dir);
        let sync_dir = TempDir::new().unwrap();

        let stored = set_sync_incoming_dir(
            &ctx,
            sync_dir.path().to_string_lossy().to_string(),
            &PathPolicy::AllowAll,
        )
        .unwrap();

        let id = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            crate::db::get_scan_roots(&conn)
                .unwrap()
                .into_iter()
                .find(|r| r.path == stored)
                .unwrap()
                .id
                .unwrap()
        };

        // plain delete is refused by the guard (same as calibration library)
        assert!(matches!(
            delete_scan_root(&ctx, id),
            Err(ApiError::Conflict(_))
        ));

        // after clear demotes it to 'normal', plain delete is allowed
        clear_sync_incoming_dir(&ctx).unwrap();
        assert!(delete_scan_root(&ctx, id).is_ok());
    }
}

/// Folders-screen redesign Task 1 — `classify_folder_candidate`, the dry-run
/// classifier behind `validate_folder_candidate`. Exercised at `Connection`
/// level with real temp dirs (the classifier canonicalizes registered paths
/// for comparison), following this module's established "test the extracted
/// real logic, not a local re-implementation" convention.
///
/// Registered roots are upserted with their CANONICALIZED path so the
/// `conflicting_path` assertions can be exact: the verdict echoes the path as
/// stored, and on macOS a tempdir's `/var/folders/…` canonicalizes to
/// `/private/var/folders/…`.
#[cfg(test)]
mod candidate_tests {
    use super::*;

    fn conn() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&c).unwrap();
        c
    }

    /// Canonicalized path string of an existing directory — used both for the
    /// `upsert_scan_root` write and the `conflicting_path` assertion.
    fn canon(path: &Path) -> String {
        path.canonicalize().unwrap().to_string_lossy().to_string()
    }

    #[test]
    fn normal_inside_existing_root_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let c = conn();
        crate::db::upsert_scan_root(&c, &canon(&root), "normal").unwrap();
        let v = classify_folder_candidate(&c, "normal", &sub.canonicalize().unwrap()).unwrap();
        assert!(!v.ok);
        assert_eq!(v.reason.as_deref(), Some("inside_existing"));
        assert_eq!(v.conflicting_path.as_deref(), Some(canon(&root).as_str()));
    }

    #[test]
    fn normal_containing_existing_root_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("parent");
        let root = parent.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let c = conn();
        crate::db::upsert_scan_root(&c, &canon(&root), "normal").unwrap();
        let v = classify_folder_candidate(&c, "normal", &parent.canonicalize().unwrap()).unwrap();
        assert_eq!(v.reason.as_deref(), Some("contains_existing"));
    }

    #[test]
    fn normal_duplicate_is_already_monitored() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let c = conn();
        crate::db::upsert_scan_root(&c, &canon(&root), "normal").unwrap();
        let v = classify_folder_candidate(&c, "normal", &root.canonicalize().unwrap()).unwrap();
        assert_eq!(v.reason.as_deref(), Some("already_monitored"));
    }

    #[test]
    fn calibration_inside_existing_is_ok_covered() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let sub = root.join("masters");
        std::fs::create_dir_all(&sub).unwrap();
        let c = conn();
        crate::db::upsert_scan_root(&c, &canon(&root), "normal").unwrap();
        let v = classify_folder_candidate(&c, "calibration_library", &sub.canonicalize().unwrap())
            .unwrap();
        assert!(v.ok);
        assert_eq!(v.placement.as_deref(), Some("covered"));
    }

    #[test]
    fn calibration_standalone_is_ok_standalone() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("masters");
        std::fs::create_dir_all(&dir).unwrap();
        let v =
            classify_folder_candidate(&conn(), "calibration_library", &dir.canonicalize().unwrap())
                .unwrap();
        assert!(v.ok);
        assert_eq!(v.placement.as_deref(), Some("standalone"));
    }

    #[test]
    fn taken_role_is_role_taken() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let c = conn();
        crate::db::upsert_scan_root(&c, &canon(&a), "sync_incoming").unwrap();
        let v = classify_folder_candidate(&c, "sync_incoming", &b.canonicalize().unwrap()).unwrap();
        assert_eq!(v.reason.as_deref(), Some("role_taken"));
        assert_eq!(v.conflicting_path.as_deref(), Some(canon(&a).as_str()));
    }

    #[test]
    fn archive_kind_skips_placement_checks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let sub = root.join("archive");
        std::fs::create_dir_all(&sub).unwrap();
        let c = conn();
        crate::db::upsert_scan_root(&c, &canon(&root), "normal").unwrap();
        let v = classify_folder_candidate(&c, "archive", &sub.canonicalize().unwrap()).unwrap();
        assert!(v.ok);
    }
}

/// Folders-screen redesign Task 2 — [`switch_calibration_library_dir`], the
/// one-step library move. Exercised at the `ServiceContext` level (real temp
/// catalog + real temp folders) because the switch composes
/// `get_calibration_library_root` → `db::delete_scan_root` →
/// `set_calibration_library_dir`, all of which stat/canonicalize on disk —
/// same rationale as `special_root_tests` above.
///
/// Folder paths are CANONICALIZED by the helper so the returned-path and
/// settings-key assertions can be exact (on macOS a tempdir's `/var/folders/…`
/// canonicalizes to `/private/var/folders/…`).
#[cfg(test)]
mod switch_library_tests {
    use super::*;

    fn ctx(tmp: &tempfile::TempDir) -> ServiceContext {
        ServiceContext::new_for_tests(tmp.path().join("catalog.db"))
    }

    fn mkdirs(tmp: &tempfile::TempDir, name: &str) -> String {
        let p = tmp.path().join(name);
        std::fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap().to_string_lossy().to_string()
    }

    #[test]
    fn standalone_to_standalone_replaces_root_and_purges_catalog() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let old = mkdirs(&tmp, "lib_old");
        let new = mkdirs(&tmp, "lib_new");
        set_calibration_library_dir(&ctx, old.clone(), &PathPolicy::AllowAll).unwrap();
        // A cataloged file under the old library — must be purged with the root.
        {
            let db = ctx.db.get().unwrap();
            db.conn()
                .execute(
                    "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'm.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
                    rusqlite::params![format!("{old}/m.fits")],
                )
                .unwrap();
        }
        let effective =
            switch_calibration_library_dir(&ctx, new.clone(), &PathPolicy::AllowAll).unwrap();
        assert_eq!(effective, new);
        let roots = get_scan_roots(&ctx).unwrap();
        let libs: Vec<_> = roots
            .iter()
            .filter(|r| r.kind == "calibration_library")
            .collect();
        assert_eq!(libs.len(), 1);
        assert_eq!(libs[0].path, new);
        assert_eq!(
            get_calibration_library_dir(&ctx).unwrap().as_deref(),
            Some(new.as_str())
        );
        let db = ctx.db.get().unwrap();
        let n: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "old library's catalog rows must be purged");
    }

    #[test]
    fn standalone_to_covered_removes_old_root_and_keeps_setting_only() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let monitored = mkdirs(&tmp, "astro");
        let covered = mkdirs(&tmp, "astro/masters");
        let old = mkdirs(&tmp, "lib_old");
        add_scan_root(&ctx, monitored.clone(), &PathPolicy::AllowAll, None).unwrap();
        set_calibration_library_dir(&ctx, old, &PathPolicy::AllowAll).unwrap();
        switch_calibration_library_dir(&ctx, covered.clone(), &PathPolicy::AllowAll).unwrap();
        let roots = get_scan_roots(&ctx).unwrap();
        assert!(
            roots.iter().all(|r| r.kind != "calibration_library"),
            "no dedicated root for a covered library"
        );
        assert_eq!(
            get_calibration_library_dir(&ctx).unwrap().as_deref(),
            Some(covered.as_str())
        );
    }

    #[test]
    fn switch_with_no_previous_library_behaves_like_set() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let new = mkdirs(&tmp, "lib");
        let effective =
            switch_calibration_library_dir(&ctx, new.clone(), &PathPolicy::AllowAll).unwrap();
        assert_eq!(effective, new);
        assert_eq!(
            get_calibration_library_dir(&ctx).unwrap().as_deref(),
            Some(new.as_str())
        );
    }

    #[test]
    fn repicking_the_same_folder_is_a_noop_keep() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let lib = mkdirs(&tmp, "lib");
        set_calibration_library_dir(&ctx, lib.clone(), &PathPolicy::AllowAll).unwrap();
        switch_calibration_library_dir(&ctx, lib.clone(), &PathPolicy::AllowAll).unwrap();
        let roots = get_scan_roots(&ctx).unwrap();
        assert_eq!(
            roots
                .iter()
                .filter(|r| r.kind == "calibration_library")
                .count(),
            1
        );
    }
}
