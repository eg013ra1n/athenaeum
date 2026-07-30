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
use crate::models::{OrphanedFile, RelinkResult, ScanRoot};
use crate::monitor::MonitorService;
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
        let existing_path = normalize_path(&Path::new(&root.path).canonicalize().map_err(|e| {
            ApiError::Internal(format!("Failed to resolve existing root path: {}", e))
        })?);

        // Check exact match
        if new_path == existing_path {
            return Err(ApiError::Conflict(
                "This directory is already being monitored".to_string(),
            ));
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
            let existing_path =
                normalize_path(&Path::new(&root.path).canonicalize().map_err(|e| {
                    ApiError::Internal(format!("Failed to resolve existing root path: {}", e))
                })?);
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
                    // Near-unreachable since Release demotes the old kind row
                    // (see `clear_calibration_library_dir`) and Change folder
                    // goes through `switch_calibration_library_dir` — but it
                    // must still name a flow that EXISTS. Release is the one
                    // action that frees the role, and it is catalog-preserving:
                    // no purge to warn about.
                    ApiError::Conflict(format!(
                        "{msg}. Release the Calibration Library role from its folder row first, then assign the new folder."
                    ))
                }
                other => other,
            })?;
    }

    let db = db(ctx)?;
    let conn = db.conn();
    ctx.settings.persist_setting(
        &conn,
        crate::settings::keys::CALIBRATION_LIBRARY_DIR,
        &path_str,
    )?;
    tracing::info!(path = %path_str, covered_by_existing_root = covered, "calibration library dir set");
    Ok(path_str)
}

/// Release the calibration-library role. Two writes, both required:
///
/// 1. The `calibration.library_dir` setting gets an EMPTY value (not a delete)
///    so the legacy `calibration_library`-root fallback stays blocked — see
///    [`get_calibration_library_dir`].
/// 2. A dedicated `calibration_library`-kind root is DEMOTED to `'normal'`
///    (same shape as [`clear_special_root_dir`]) — the released folder becomes
///    a plain monitored directory (spec §5.2), where the regular scan-root list
///    can remove it and its catalog-purge consequences are already understood.
///
/// Never deletes a scan root — Release is catalog-preserving. The demote is not
/// cosmetic: the Folders rail routes every non-normal root to a Special-roles
/// row with no Remove, so a surviving kind row would strand the released folder
/// there while the role reads as free, and the next assignment would die on
/// `check_special_root_uniqueness`.
pub fn clear_calibration_library_dir(ctx: &ServiceContext) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    ctx.settings
        .persist_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR, "")?;
    let demoted = conn.execute(
        "UPDATE scan_roots SET kind = 'normal' WHERE kind = 'calibration_library'",
        [],
    )?;
    tracing::info!(demoted_roots = demoted, "calibration library dir cleared");
    Ok(())
}

/// Placement pre-flight for [`switch_calibration_library_dir`] — answers
/// "where would `new_path` land?" against every root EXCEPT the one about to
/// be replaced (`excluded_root_id`), WITHOUT writing. Returns `covered` (the
/// new folder is equal to or nested inside a surviving root, so the switch is
/// settings-key-only) or an error the delegate would have raised anyway.
///
/// Its whole reason to exist is ORDERING: `set_calibration_library_dir`'s own
/// coverage probe hard-errors on any registered root it cannot `canonicalize`
/// (an offline drive), and its standalone branch rejects a folder that
/// swallows an existing root — both AFTER the delete would already have
/// purged the old library's catalog rows. Running the same checks here moves
/// those failures in front of the destructive step.
///
/// Not `classify_folder_candidate`: that one's `role_taken` arm would trip on
/// the very root being replaced.
///
/// Two passes, deliberately: `covered` is decided over ALL surviving roots
/// before any conflict is raised, so this can never reject a placement the
/// delegate would have accepted (with valid non-overlapping roots the two
/// passes cannot both fire; the ordering makes that robust anyway).
fn preflight_library_placement(
    conn: &rusqlite::Connection,
    new_path: &Path,
    excluded_root_id: Option<i64>,
) -> Result<bool, ApiError> {
    let mut survivors = Vec::new();
    for root in crate::db::get_scan_roots(conn)? {
        if excluded_root_id.is_some() && root.id == excluded_root_id {
            continue;
        }
        // Same canonicalize-or-hard-error semantics as the delegate's probe:
        // an unresolvable root is a real failure, it just belongs HERE.
        let canon = normalize_path(&Path::new(&root.path).canonicalize().map_err(|e| {
            ApiError::Internal(format!("Failed to resolve existing root path: {}", e))
        })?);
        survivors.push((root.path, canon));
    }

    // `starts_with` is true for equality too — an equal-or-nested folder is
    // COVERED by that root (settings-key-only placement), never a conflict.
    let covered = survivors
        .iter()
        .any(|(_, canon)| new_path.starts_with(canon));
    if covered {
        return Ok(true);
    }

    // Standalone placement: the folder will become its own scan root, so it
    // must not swallow an existing one. Same message `add_scan_root` raises.
    for (raw, canon) in survivors.iter() {
        if canon.starts_with(new_path) {
            return Err(ApiError::Conflict(format!(
                "Cannot add directory: existing scan root '{}' is a subdirectory of it",
                raw
            )));
        }
    }
    Ok(false)
}

/// One-step calibration-library move (spec §8.1): validates the new path,
/// removes the old dedicated `calibration_library` root (catalog purge — same
/// semantics as deleting it from the folder list; files on disk untouched),
/// then delegates to [`set_calibration_library_dir`] for the covered/standalone
/// placement of the new folder. Replaces the old clear → remove-root → set
/// dance.
///
/// Deliberately bypasses [`guard_against_special_root_deletion`]: the guard
/// exists to stop an operator removing the library out from under the role;
/// here removing it IS the requested operation.
///
/// **Validate before destroy.** The new path's placement is resolved by
/// [`preflight_library_placement`] against the roots that will survive, BEFORE
/// the delete — otherwise a switch that the delegate was always going to
/// reject (offline registered root, or a folder containing an existing root)
/// would purge the old library's catalog rows on its way to failing. Residual
/// window: the two phases are still not one transaction, so a delegate failure
/// after a passed pre-flight means a rare TOCTOU (the disk changed in between).
/// If that happens the old root is gone; IF the `calibration.library_dir`
/// setting was the one naming it (equal to it, or nested under it), it is
/// best-effort demoted to `""` (a failure to clear is logged) and the role
/// reads unassigned — a key that names something else entirely (e.g. a still-
/// working covered library, orphaned only because an unrelated vestigial
/// `calibration_library`-kind root got deleted in the same call) is left
/// untouched. Designate again to recover. The error from the set phase
/// surfaces verbatim.
///
/// Re-picking the folder that is already the library is a no-op keep: the old
/// root is left alone (it equals the new path), and the delegate's
/// covered-placement branch simply re-persists the settings key — so the
/// single-library uniqueness check is never reached.
///
/// Note for confirmation-dialog copy: the purge target is the
/// `calibration_library`-KIND root, which is not necessarily the effective
/// library dir. For a covered library the kind root can be an ancestor of it
/// (or an unrelated vestigial root), so the purge scope can exceed the
/// displayed library path — name [`get_calibration_library_root`]'s path in
/// the warning, not the resolved library dir.
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
    let mut removed_root_path: Option<String> = None;
    if let Some(old) = old_root {
        if canonical_or_raw(&old.path) != new_path {
            let id = old.id.ok_or_else(|| {
                ApiError::Internal("calibration library root has no id".to_string())
            })?;
            let db = db(ctx)?;
            let conn = db.conn();
            let covered = preflight_library_placement(&conn, &new_path, Some(id))?;
            tracing::info!(src = %old.path, dest = %new_path.display(), covered_by_existing_root = covered, "switching calibration library — removing old dedicated root");
            crate::db::delete_scan_root(&conn, id).map_err(|e| {
                tracing::error!(root_id = id, error = %e, "failed to remove old calibration library root");
                ApiError::Internal(format!("Failed to remove old calibration library root: {e}"))
            })?;
            removed_root_path = Some(old.path.clone());
        }
    }

    set_calibration_library_dir(ctx, new_path.to_string_lossy().to_string(), policy).map_err(|e| {
        if let Some(removed) = removed_root_path.as_deref() {
            // The root we just deleted is gone — but the settings key is a
            // SECOND, independent copy of a path, and `get_calibration_library_root`
            // only returns the FIRST `calibration_library`-kind row: a vestigial
            // second row (Minor-5, deliberately left in place) can be the one
            // that got deleted here while the working key names a completely
            // different, still-intact covered library. Only demote when the
            // CURRENT resolved key is actually the one we just orphaned — equal
            // to the removed root's path, or nested under it (same
            // separator-strict prefix test the relink guard uses) — never an
            // unrelated key.
            match db(ctx) {
                Ok(db) => {
                    let conn = db.conn();
                    match resolve_calibration_library_dir(&conn) {
                        Ok(Some(dir)) => {
                            let sep = if removed.starts_with('/') { '/' } else { '\\' };
                            let trimmed = removed.trim_end_matches(sep);
                            let removed_prefix = format!("{}{}", trimmed, sep);
                            if dir == *removed || dir.starts_with(&removed_prefix) {
                                // The old root is already purged; a key naming
                                // it is the one state no screen can explain.
                                // Demote to "role unassigned" — designating
                                // again is the documented recovery.
                                if let Err(e2) = ctx.settings.persist_setting(
                                    &conn,
                                    crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                                    "",
                                ) {
                                    tracing::error!(error = %e2, "failed to clear stale calibration library key after failed switch");
                                }
                                tracing::error!(error = %e, "library switch failed after old root removal — role left unassigned");
                            }
                        }
                        Ok(None) => {
                            // Already empty/unset — nothing to demote.
                        }
                        Err(e2) => {
                            tracing::error!(error = %e2, "failed to resolve calibration library dir while checking demote gate after failed switch");
                        }
                    }
                }
                Err(e2) => {
                    tracing::error!(error = %e2, "no db handle to check/clear stale calibration library key after failed switch");
                }
            }
        }
        e
    })
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
                "This directory contains your Calibration Folder ({lib_dir}). Clear or move the Calibration Folder in File Manager → Folders first, then remove the directory."
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
                    "This directory is your {label} ({dir}). Clear the {label} in File Manager → Folders first, then remove the directory."
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
        Path::new(&root.path),
        &conn,
        None,
        true,
        root.unique_camera,
        root_id,
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
                            tracing::debug!(
                                current = updated + skipped + missing,
                                total,
                                "content hash rescan progress"
                            );
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
    let result = crate::relinking::relink_files(&conn, &old_path, &new_path).map_err(|e| {
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

        // The calibration library is the only role stored as a SECOND copy
        // of a path (settings key) rather than as the root row itself — a
        // relink that moves the covering root must move the key too, or it
        // orphans: masters keep landing at the old path and the Folders
        // rail loses the covered-library row.
        if let Some(dir) = resolve_calibration_library_dir(&conn)? {
            let sep = if old_path.starts_with('/') { '/' } else { '\\' };
            let trimmed = old_path.trim_end_matches(sep);
            let old_prefix = format!("{}{}", trimmed, sep);
            if dir == old_path || dir.starts_with(&old_prefix) {
                // Slice at the TRIMMED length, not `old_path.len()`: a
                // (defensive-only — paths are canonicalized on write)
                // trailing-separator `old_path` would otherwise either eat the
                // remainder's leading separator (`/new/rootmasters`) or, with
                // multiple trailing separators, slice past a char boundary.
                let moved = format!("{}{}", new_path, &dir[trimmed.len()..]);
                ctx.settings
                    .persist_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR, &moved)
                    .map_err(|e| {
                        tracing::error!(root_id, error = %e, "failed to move calibration library key with relinked root");
                        ApiError::Internal(format!("Relinked, but failed to move the Calibration Library setting: {e}"))
                    })?;
                tracing::info!(root_id, src = %dir, dest = %moved, "calibration library dir followed relinked root");
            }
        }
    }

    Ok(result)
}

/// Check availability of all scan roots
pub fn check_all_scan_roots_availability(
    ctx: &ServiceContext,
) -> Result<Vec<(i64, bool)>, ApiError> {
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
        let mut stmt = conn.prepare(
            "SELECT f.id, f.path, f.filename, f.size, f.modified_at,
                        CASE WHEN fr.id IS NOT NULL THEN 1 ELSE 0 END as has_frame,
                        fr.object,
                        fr.date_obs
                 FROM files f
                 LEFT JOIN frames fr ON fr.file_id = f.id
                 WHERE f.path LIKE ?1 AND f.archived_in_operation IS NULL",
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
    crate::scanner::emit_progress(
        emitter,
        root_id,
        total_files,
        total_files,
        None,
        "verifying",
    );

    Ok(missing_files)
}

/// Toggle unique_camera flag (flag-only, cascade happens on re-scan).
///
/// Web-side counterpart lives in `routes/duplicates.rs` (not
/// `routes/scan_roots.rs`) and was NOT converted in Task 9 — out of that
/// file's declared scope; see Task 9 report for the gap.
pub fn set_scan_root_unique_camera_flag(
    ctx: &ServiceContext,
    id: i64,
    enabled: bool,
) -> Result<(), ApiError> {
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

    tracing::info!(
        root_id = id,
        monitor_enabled = enabled,
        "scan root monitor toggled"
    );

    // Wake the monitor loop so the user gets an immediate scan instead of
    // waiting for the current sleep to finish. Only relevant when enabling.
    if enabled {
        monitor.kick();
    }

    Ok(())
}

/// Calibration-frame `(frame_id, imagetyp)` rows under `root_path`,
/// separator-strict. Extracted from `recreate_calibration_sets_for_root` so
/// the sibling-root boundary is unit-testable without driving the whole
/// set-rebuild machinery. The delete step this rebuild pairs with
/// (`db::delete_calibration_sets_for_root`) has been byte-range-strict since
/// 81aedae7 — the rebuild sweeping wider than the delete folded a sibling
/// root's darks/flats into this root's rebuilt sets.
fn calibration_frame_rows_under_root(
    conn: &rusqlite::Connection,
    root_path: &str,
) -> Result<Vec<(i64, String)>, rusqlite::Error> {
    let (pred, values) = crate::db::scan_root_prefix_predicate("f.path", &[root_path.to_string()]);
    let sql = format!(
        "SELECT fr.id, fr.imagetyp FROM frames fr
         JOIN files f ON fr.file_id = f.id
         WHERE ({pred})
           AND fr.imagetyp IN ('Flat','Dark','Bias','DarkFlat','MasterFlat','MasterDark','MasterBias','MasterDarkFlat')"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values.iter()), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect()
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

    let mut flat_ids = Vec::new();
    let mut dark_ids = Vec::new();
    let mut bias_ids = Vec::new();
    let mut darkflat_ids = Vec::new();
    let mut master_ids = MasterFrameIds::default();

    // Query all calibration frame IDs under this root, grouped by imagetyp
    let rows = calibration_frame_rows_under_root(conn, &root_path)?;

    for (frame_id, imagetyp) in rows {
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

    let total_cal_frames = flat_ids.len()
        + dark_ids.len()
        + bias_ids.len()
        + darkflat_ids.len()
        + master_ids.total_count();

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
        conn,
        flat_ids,
        dark_ids,
        bias_ids,
        darkflat_ids,
        master_ids,
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
        Err(ApiError::NotFound(
            "No active scan for this root".to_string(),
        ))
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

/// Per-folder stats for the Folders rail/inspector (spec §8.3) — one call,
/// no N+1 from the frontend.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
pub struct ScanRootOverview {
    pub root_id: i64,
    pub file_count: i64,
    pub total_bytes: i64,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
pub struct ArchiveRootOverview {
    pub archive_root_id: i64,
    pub path: String,
    /// Zipped FRAME SETS under this root (`frames_set.archived_at IS NOT NULL`
    /// whose operation targeted this root). Phase-2 calibration-originals
    /// archives (`archive_operations.calibration_set_id`, no `frames_set` link)
    /// are deliberately excluded here AND from [`Self::total_zip_bytes`], so
    /// the two numbers describe the same subject as the Folders inspector's
    /// Contents list. Named follow-up if the UI ever surfaces calibration
    /// archives: give them their own count/bytes pair rather than folding them
    /// into these.
    pub set_count: i64,
    /// Sum of on-disk sizes of the distinct zips recorded for this root's
    /// frame-set operations; missing zips contribute 0 (mirrors
    /// `list_archive_zips`). Calibration-originals archives are excluded for
    /// the reason documented on [`Self::set_count`].
    pub total_zip_bytes: i64,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
pub struct FolderOverview {
    pub scan_roots: Vec<ScanRootOverview>,
    pub archive_roots: Vec<ArchiveRootOverview>,
}

/// On-disk size of one archive zip, or 0 when it cannot be measured. A missing
/// file is an expected state (the zip was deleted, or the archive volume is
/// detached) and stays silent; any other error still contributes 0 but is
/// logged, because a total that silently shrinks otherwise reads as data loss.
fn zip_size_bytes(path: &str) -> i64 {
    match std::fs::metadata(path) {
        Ok(m) => m.len() as i64,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => {
            tracing::warn!(path = %path, error = %e, "zip size unavailable");
            0
        }
    }
}

pub fn get_folder_overview(ctx: &ServiceContext) -> Result<FolderOverview, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let mut scan_roots = Vec::new();
    for root in crate::db::get_scan_roots(&conn)? {
        let Some(root_id) = root.id else { continue };
        // Separator-safe byte-range prefix match, one arm per native separator
        // (catalog paths are stored as absolute native paths, so a POSIX and a
        // Windows root can both live in one catalog — see
        // `db::operations::native_separator_of`). The upper bound bumps the
        // SEPARATOR by one code point (`'/'`+1 = `'0'`, `'\'`+1 = `']'`), so
        // the range is exactly `<root><sep>…`: `/data/Set1` still excludes
        // `/data/Set10`, and — unlike the `LIKE ?1 || '/%'` form this replaces
        // — `_`/`%` inside a folder name stay literal instead of acting as
        // wildcards. Exact-case, index-seekable on `idx_files_path`; same
        // semantics as `db::operations::scan_root_prefix_predicate`, which is
        // separator-strict too (both close the same name-prefix-sibling
        // hazard — see the `name_prefix_sibling` tests in `db/operations.rs`).
        let (file_count, total_bytes): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM files
                 WHERE (path >= ?1 || '/' AND path < ?1 || '0')
                    OR (path >= ?1 || '\\' AND path < ?1 || ']')",
                rusqlite::params![root.path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| ApiError::Internal(format!("overview query failed: {e}")))?;
        scan_roots.push(ScanRootOverview {
            root_id,
            file_count,
            total_bytes,
        });
    }

    let mut archive_roots = Vec::new();
    let mut stmt = conn
        .prepare("SELECT id, path FROM archive_roots ORDER BY id")
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let roots: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .collect::<rusqlite::Result<_>>()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    // Prepared once and reused per root — `query_map` resets the statement.
    // The `frames_set` join mirrors the `set_count` query above it: both
    // fields must describe frame-set archives only (see `set_count`'s doc).
    let mut zstmt = conn
        .prepare(
            "SELECT DISTINCT aof.target_zip_path FROM archive_operation_files aof
             JOIN archive_operations op ON aof.operation_id = op.id
             JOIN frames_set fs ON fs.archive_operation_id = op.id
                                AND fs.archived_at IS NOT NULL
             WHERE op.archive_root_path = ?1",
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    for (id, path) in roots {
        let set_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM frames_set fs
                 JOIN archive_operations op ON fs.archive_operation_id = op.id
                 WHERE fs.archived_at IS NOT NULL AND op.archive_root_path = ?1",
                rusqlite::params![path],
                |row| row.get(0),
            )
            .map_err(|e| ApiError::Internal(format!("archive set count failed: {e}")))?;
        let zips: Vec<String> = zstmt
            .query_map(rusqlite::params![path], |row| row.get(0))
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .collect::<rusqlite::Result<_>>()
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let total_zip_bytes = zips.iter().map(|z| zip_size_bytes(z)).sum();
        archive_roots.push(ArchiveRootOverview {
            archive_root_id: id,
            path,
            set_count,
            total_zip_bytes,
        });
    }

    Ok(FolderOverview {
        scan_roots,
        archive_roots,
    })
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
        let root_path = root
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
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
        let root_a_path = root_a
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
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
        let lib_root_path = lib_root
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
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
        let old_lib_root_path = old_lib_root
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
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
        let root_path = root
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
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

    fn insert_file(ctx: &ServiceContext, path: &str) {
        let db = ctx.db.get().unwrap();
        db.conn()
            .execute(
                "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'm.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![path],
            )
            .unwrap();
    }

    fn file_paths(ctx: &ServiceContext) -> Vec<String> {
        let db = ctx.db.get().unwrap();
        let conn = db.conn();
        let mut stmt = conn
            .prepare("SELECT path FROM files ORDER BY path")
            .unwrap();
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows
    }

    fn library_root_paths(ctx: &ServiceContext) -> Vec<String> {
        get_scan_roots(ctx)
            .unwrap()
            .into_iter()
            .filter(|r| r.kind == "calibration_library")
            .map(|r| r.path)
            .collect()
    }

    #[test]
    fn standalone_to_standalone_replaces_root_and_purges_catalog() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let old = mkdirs(&tmp, "lib_old");
        let new = mkdirs(&tmp, "lib_new");
        set_calibration_library_dir(&ctx, old.clone(), &PathPolicy::AllowAll).unwrap();
        // A cataloged file under the old library — must be purged with the root.
        insert_file(&ctx, &format!("{old}/m.fits"));
        // …and one OUTSIDE it, which must survive: the purge is a path-prefix
        // byte-range delete, not a table wipe. `elsewhere` also sorts below
        // `lib_old` (`e` < `l`), so it is outside the prefix range as well as
        // outside the subtree.
        let keep = format!("{}/keep.fits", mkdirs(&tmp, "elsewhere"));
        insert_file(&ctx, &keep);
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
        assert_eq!(
            file_paths(&ctx),
            vec![keep],
            "only the old library's own catalog rows may be purged"
        );
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

    // ── Validate-before-delete (Important-1 fix round) ───────────────────
    //
    // Both cases fail INSIDE the delegated `set_calibration_library_dir`, so
    // before the pre-flight they failed only AFTER the old root had already
    // been deleted and its catalog rows purged. The intactness assertions are
    // the point of these tests — the error-kind assertions alone passed even
    // with the destructive ordering.

    #[test]
    fn unresolvable_registered_root_aborts_before_the_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let old = mkdirs(&tmp, "lib_old");
        let new = mkdirs(&tmp, "lib_new");
        set_calibration_library_dir(&ctx, old.clone(), &PathPolicy::AllowAll).unwrap();
        insert_file(&ctx, &format!("{old}/m.fits"));
        // A monitored root on disconnected storage: `canonicalize()` fails on
        // it, so the placement probe hard-errors. Inserted directly — the
        // public add path stat-checks the folder and would refuse it.
        {
            let db = ctx.db.get().unwrap();
            crate::db::upsert_scan_root(
                &db.conn(),
                &tmp.path().join("offline_root").to_string_lossy(),
                "normal",
            )
            .unwrap();
        }

        let err = switch_calibration_library_dir(&ctx, new, &PathPolicy::AllowAll)
            .expect_err("an unresolvable registered root must abort the switch");
        assert!(matches!(err, ApiError::Internal(_)), "unexpected: {err:?}");
        assert_eq!(
            library_root_paths(&ctx),
            vec![old.clone()],
            "the old library root must survive a rejected switch"
        );
        assert_eq!(
            file_paths(&ctx),
            vec![format!("{old}/m.fits")],
            "a rejected switch must not purge the old library's catalog rows"
        );
        assert_eq!(
            get_calibration_library_dir(&ctx).unwrap().as_deref(),
            Some(old.as_str())
        );
    }

    #[test]
    fn folder_containing_an_existing_root_aborts_before_the_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let old = mkdirs(&tmp, "lib_old");
        let parent = mkdirs(&tmp, "parent");
        let child = mkdirs(&tmp, "parent/child");
        add_scan_root(&ctx, child, &PathPolicy::AllowAll, None).unwrap();
        set_calibration_library_dir(&ctx, old.clone(), &PathPolicy::AllowAll).unwrap();
        insert_file(&ctx, &format!("{old}/m.fits"));

        let err = switch_calibration_library_dir(&ctx, parent, &PathPolicy::AllowAll)
            .expect_err("a folder containing an existing root must abort the switch");
        assert!(matches!(err, ApiError::Conflict(_)), "unexpected: {err:?}");
        assert_eq!(
            library_root_paths(&ctx),
            vec![old.clone()],
            "the old library root must survive a rejected switch"
        );
        assert_eq!(
            file_paths(&ctx),
            vec![format!("{old}/m.fits")],
            "a rejected switch must not purge the old library's catalog rows"
        );
    }

    /// The pre-flight's `excluded_root_id` is load-bearing, not cosmetic:
    /// without it, moving the library OFF a disconnected drive would abort on
    /// the old root's own unresolvable path — the one case where the operator
    /// most needs the switch to work. Registered directly (the public add path
    /// stat-checks the folder).
    #[test]
    fn switching_away_from_an_offline_library_root_is_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let new = mkdirs(&tmp, "lib_new");
        let offline = tmp.path().join("offline_lib").to_string_lossy().to_string();
        {
            let db = ctx.db.get().unwrap();
            let conn = db.conn();
            crate::db::upsert_scan_root(&conn, &offline, "calibration_library").unwrap();
            ctx.settings
                .persist_setting(
                    &conn,
                    crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                    &offline,
                )
                .unwrap();
        }

        let effective =
            switch_calibration_library_dir(&ctx, new.clone(), &PathPolicy::AllowAll).unwrap();
        assert_eq!(effective, new);
        assert_eq!(library_root_paths(&ctx), vec![new.clone()]);
        assert_eq!(
            get_calibration_library_dir(&ctx).unwrap().as_deref(),
            Some(new.as_str())
        );
    }

    // ── Release (Critical-1 fix round) ───────────────────────────────────
    //
    // Shares this module's ServiceContext fixtures. Clearing a STANDALONE
    // library used to write the empty settings key and stop there, leaving the
    // `calibration_library`-kind row behind: the Folders rail routes every
    // non-normal root to a Special-roles entry (no Remove), so the released
    // folder stayed pinned there forever while the dialog reported the role
    // free — and re-assigning died on `check_special_root_uniqueness`.
    #[test]
    fn releasing_a_standalone_library_demotes_the_root_and_frees_the_role() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let lib = mkdirs(&tmp, "lib_old");
        set_calibration_library_dir(&ctx, lib.clone(), &PathPolicy::AllowAll).unwrap();
        // A cataloged file under it — Release must never purge anything.
        insert_file(&ctx, &format!("{lib}/m.fits"));

        clear_calibration_library_dir(&ctx).unwrap();

        let roots = get_scan_roots(&ctx).unwrap();
        let released = roots
            .iter()
            .find(|r| r.path == lib)
            .expect("Release never deletes the scan root — the folder stays monitored");
        assert_eq!(
            released.kind, "normal",
            "the released folder must be demoted to a plain monitored directory"
        );
        assert!(
            library_root_paths(&ctx).is_empty(),
            "no calibration_library-kind root may survive a Release"
        );
        assert_eq!(get_calibration_library_dir(&ctx).unwrap(), None);
        assert_eq!(
            file_paths(&ctx),
            vec![format!("{lib}/m.fits")],
            "Release is catalog-preserving — it is not a removal"
        );

        // …and the role is genuinely free again: a NEW standalone folder can
        // take it without tripping the single-library uniqueness Conflict.
        let next = mkdirs(&tmp, "lib_new");
        let effective =
            set_calibration_library_dir(&ctx, next.clone(), &PathPolicy::AllowAll).unwrap();
        assert_eq!(effective, next);
        assert_eq!(library_root_paths(&ctx), vec![next.clone()]);
        assert_eq!(
            get_calibration_library_dir(&ctx).unwrap().as_deref(),
            Some(next.as_str())
        );
    }

    // ── Relink-follows-root (Task 2 fix round) ────────────────────────────
    //
    // Relink is the one write path that moves a covering root's `path`
    // without going through this module's set/switch/clear trio — so the
    // `calibration.library_dir` settings key (a SECOND copy of a path, not
    // derived from the root row) needs its own follow logic here.

    #[test]
    fn relink_scan_root_carries_calibration_library_key() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let root_id: i64;
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            conn.execute("INSERT INTO scan_roots (path) VALUES ('/gone/root')", [])
                .unwrap();
            root_id = conn
                .query_row(
                    "SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                "/gone/root/masters",
            )
            .unwrap();
        }
        let new = mkdirs(&tmp, "newroot");

        relink_scan_root(&ctx, root_id, new.clone(), &PathPolicy::AllowAll).unwrap();

        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let got =
            crate::db::get_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR).unwrap();
        assert_eq!(
            got.as_deref(),
            Some(format!("{new}/masters").as_str()),
            "library key must follow the relinked covering root"
        );
    }

    #[test]
    fn relink_scan_root_leaves_unrelated_calibration_library_key_alone() {
        // Name-prefix sibling of the relinked root: /gone/rootX is NOT
        // covered by /gone/root — the key must not move.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let root_id: i64;
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            conn.execute("INSERT INTO scan_roots (path) VALUES ('/gone/root')", [])
                .unwrap();
            root_id = conn
                .query_row(
                    "SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                "/gone/rootX/masters",
            )
            .unwrap();
        }
        let new = mkdirs(&tmp, "newroot");

        relink_scan_root(&ctx, root_id, new, &PathPolicy::AllowAll).unwrap();

        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let got =
            crate::db::get_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR).unwrap();
        assert_eq!(got.as_deref(), Some("/gone/rootX/masters"));
    }

    // ── Switch-failure demote (Task 2 fix round, closes the #8 residual) ──
    //
    // Deterministic delegate-fails-after-delete lever: TWO calibration_library
    // kind rows. `switch_calibration_library_dir`'s own preflight has no
    // uniqueness arm (that lives in `check_special_root_uniqueness`, reached
    // only inside the delegated `set_calibration_library_dir` → `add_scan_root`
    // call), so the old root is purged before the delegate rejects the second
    // standalone root. The key must demote to "" rather than keep naming the
    // now-removed folder.

    #[test]
    fn switch_failure_after_delete_demotes_key_instead_of_orphaning() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let old = mkdirs(&tmp, "lib_old");
        let stray = mkdirs(&tmp, "lib_stray");
        let new = mkdirs(&tmp, "lib_new");
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            conn.execute(
                "INSERT INTO scan_roots (path, kind) VALUES (?1, 'calibration_library'), (?2, 'calibration_library')",
                rusqlite::params![old, stray],
            )
            .unwrap();
            crate::db::set_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR, &old)
                .unwrap();
        }

        let err = switch_calibration_library_dir(&ctx, new, &PathPolicy::AllowAll);

        assert!(
            err.is_err(),
            "delegate uniqueness check must reject the stray second library root"
        );
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let got =
            crate::db::get_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR).unwrap();
        assert_eq!(
            got.as_deref(),
            Some(""),
            "key must be demoted, not orphaned"
        );
    }

    /// Reviewer-flagged Important fix: `get_calibration_library_root` returns
    /// only the FIRST `calibration_library`-kind row it finds. With two such
    /// rows present (Minor-5's vestigial-row state), deleting the FIRST one
    /// during a switch must not demote a settings key that names a completely
    /// different, still-working covered library. Negative-space companion to
    /// `switch_failure_after_delete_demotes_key_instead_of_orphaning` above —
    /// this is the case where the demote gate must stay CLOSED.
    #[test]
    fn switch_failure_after_delete_leaves_unrelated_covered_key_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let monitored = mkdirs(&tmp, "astro");
        let covered = mkdirs(&tmp, "astro/masters");
        let vestige1 = mkdirs(&tmp, "vestige1");
        let vestige2 = mkdirs(&tmp, "vestige2");
        let new = mkdirs(&tmp, "lib_new");
        add_scan_root(&ctx, monitored, &PathPolicy::AllowAll, None).unwrap();
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            conn.execute(
                "INSERT INTO scan_roots (path, kind) VALUES (?1, 'calibration_library'), (?2, 'calibration_library')",
                rusqlite::params![vestige1, vestige2],
            )
            .unwrap();
            // The WORKING key: a covered library under the monitored root —
            // unrelated to either vestigial dedicated root.
            crate::db::set_setting(
                &conn,
                crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                &covered,
            )
            .unwrap();
        }

        let err = switch_calibration_library_dir(&ctx, new, &PathPolicy::AllowAll);

        assert!(
            err.is_err(),
            "delegate uniqueness check must still reject the surviving vestigial root"
        );
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let got =
            crate::db::get_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR).unwrap();
        assert_eq!(
            got.as_deref(),
            Some(covered.as_str()),
            "a vestigial root's deletion must never clear an unrelated, still-working covered-library key"
        );
    }

    /// Negative fixture (Minor-2): with NO `calibration_library`-kind root at
    /// all, `get_calibration_library_root` returns `None`, so the delete
    /// branch never runs and `removed_root_path` stays `None` — the demote
    /// gate must never even be consulted. Without the `if let Some(removed)`
    /// gate this would still pass (there's no delete to react to), but it
    /// pins the baseline the Important fix's `Some` branch is layered on top
    /// of, so a future refactor that starts demoting unconditionally trips it.
    #[test]
    fn switch_failure_without_any_previous_library_root_never_touches_an_unrelated_key() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp);
        let monitored = mkdirs(&tmp, "astro");
        let covered = mkdirs(&tmp, "astro/masters");
        let other = mkdirs(&tmp, "other");
        let child = mkdirs(&tmp, "other/child");
        add_scan_root(&ctx, monitored, &PathPolicy::AllowAll, None).unwrap();
        add_scan_root(&ctx, child.clone(), &PathPolicy::AllowAll, None).unwrap();
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                &covered,
            )
            .unwrap();
        }
        // No calibration_library-kind root exists at all — get_calibration_library_root
        // returns None, so switch's delete branch is never entered.
        assert!(get_calibration_library_root(&ctx).unwrap().is_none());

        let err = switch_calibration_library_dir(&ctx, other, &PathPolicy::AllowAll);

        assert!(
            err.is_err(),
            "a folder containing an existing root ('{child}') must be rejected"
        );
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let got =
            crate::db::get_setting(&conn, crate::settings::keys::CALIBRATION_LIBRARY_DIR).unwrap();
        assert_eq!(
            got.as_deref(),
            Some(covered.as_str()),
            "no root was ever deleted — the key must be untouched"
        );
    }
}

/// Task 3 — [`get_folder_overview`] aggregates. Runs against a real
/// `ServiceContext` (the handler walks `db::get_scan_roots`, so a bare
/// `Connection` would not exercise it) and pins the separator-safe prefix
/// match with a sibling row whose path shares the root's prefix without the
/// separator (`<root>2/x.fits`) — a naive `LIKE root || '%'` would fold its
/// 999 bytes into the total.
#[cfg(test)]
mod overview_tests {
    use super::*;

    #[test]
    fn overview_counts_files_and_bytes_per_root() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let root = tmp.path().join("astro");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap().to_string_lossy().to_string();
        let added = add_scan_root(&ctx, root.clone(), &PathPolicy::AllowAll, None).unwrap();
        {
            let db = ctx.db.get().unwrap();
            let conn = db.conn();
            for (name, size) in [("a.fits", 100_i64), ("b.fits", 50)] {
                conn.execute(
                    "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, '2026-01-01T00:00:00Z', 'FITS')",
                    rusqlite::params![format!("{root}/{name}"), name, size],
                )
                .unwrap();
            }
            // Boundary trap: a sibling whose path shares the prefix without the separator.
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'x.fits', 999, '2026-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![format!("{root}2/x.fits")],
            )
            .unwrap();
            // Exact-boundary pin (task-1-brief §6): the POSIX arm's upper
            // bound is `root || '0'` ('/' + 1 == '0'), so a path starting
            // with exactly that byte string is the tightest possible probe
            // of the bound itself — an off-by-one in the upper-bound literal
            // or a `<=` typo would let this leak in where `root2/...` above
            // would not.
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'x.fits', 999, '2026-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![format!("{root}0/x.fits")],
            )
            .unwrap();
        }
        let ov = get_folder_overview(&ctx).unwrap();
        let s = ov
            .scan_roots
            .iter()
            .find(|s| s.root_id == added.id.unwrap())
            .unwrap();
        assert_eq!(
            s.file_count, 2,
            "neither boundary-trap sibling must be counted"
        );
        assert_eq!(s.total_bytes, 150);
        assert!(ov.archive_roots.is_empty());
    }

    /// Review fix (Important-1): the root predicate must compare bytes, not
    /// match a `LIKE` pattern. `_` is a LIKE single-character wildcard, so a
    /// perfectly ordinary root name like `my_astro` used to also count files
    /// under a sibling `myXastro` (`%` in a folder name is the same hole,
    /// unbounded). Stats are display-only, but folder names with underscores
    /// are the norm, so this is pinned.
    #[test]
    fn overview_prefix_match_treats_underscore_in_root_as_literal() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let root = tmp.path().join("my_astro");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap().to_string_lossy().to_string();
        let added = add_scan_root(&ctx, root.clone(), &PathPolicy::AllowAll, None).unwrap();
        // Sibling that differs from the root only where the `_` sits — derived
        // from the canonicalized root's parent so the two are true siblings.
        let parent = Path::new(&root)
            .parent()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let sibling = format!("{parent}/myXastro");
        {
            let db = ctx.db.get().unwrap();
            let conn = db.conn();
            for (path, name, size) in [
                (format!("{root}/a.fits"), "a.fits", 100_i64),
                (format!("{sibling}/x.fits"), "x.fits", 999),
            ] {
                conn.execute(
                    "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, '2026-01-01T00:00:00Z', 'FITS')",
                    rusqlite::params![path, name, size],
                )
                .unwrap();
            }
        }
        let ov = get_folder_overview(&ctx).unwrap();
        let s = ov
            .scan_roots
            .iter()
            .find(|s| s.root_id == added.id.unwrap())
            .unwrap();
        assert_eq!(
            s.file_count, 1,
            "sibling myXastro/x.fits must not be counted"
        );
        assert_eq!(s.total_bytes, 100);
    }

    /// The byte-range predicate's second arm — the one keyed on `\` — with
    /// Windows-shaped fixtures, runnable from any host (same approach as
    /// `db::operations::native_separator_of`, whose doc calls out that catalog
    /// paths are native and the leading character decides). Rows are written
    /// straight to `scan_roots`/`files`: `add_scan_root` canonicalizes a real
    /// directory, which cannot produce a `C:\…` path on a POSIX host, and the
    /// handler itself never canonicalizes. Also re-checks the boundary trap on
    /// this arm (`C:\Astro2` must stay out of `C:\Astro`).
    #[test]
    fn overview_prefix_match_covers_windows_separator_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let root = r"C:\Astro";
        {
            let db = ctx.db.get().unwrap();
            let conn = db.conn();
            crate::db::upsert_scan_root(&conn, root, "normal").unwrap();
            for (path, name, size) in [
                (r"C:\Astro\a.fits", "a.fits", 100_i64),
                (r"C:\Astro\sub\b.fits", "b.fits", 50),
                (r"C:\Astro2\x.fits", "x.fits", 999),
                // Exact-boundary pin (task-1-brief §6): the Windows arm's
                // upper bound is `root || ']'` ('\' + 1 == ']'), so a path
                // starting with exactly that byte string is the tightest
                // possible probe of the bound itself.
                (r"C:\Astro]x\f.fits", "f.fits", 999),
            ] {
                conn.execute(
                    "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, '2026-01-01T00:00:00Z', 'FITS')",
                    rusqlite::params![path, name, size],
                )
                .unwrap();
            }
        }
        let ov = get_folder_overview(&ctx).unwrap();
        let s = ov
            .scan_roots
            .iter()
            .find(|s| s.root_id == get_scan_roots(&ctx).unwrap()[0].id.unwrap())
            .unwrap();
        assert_eq!(
            s.file_count, 2,
            r"neither C:\Astro2\x.fits nor C:\Astro]x\f.fits must be counted"
        );
        assert_eq!(s.total_bytes, 150);
    }

    /// The archive branch of the same handler, against real rows: the two
    /// per-root queries are only reached when `archive_roots` is non-empty, so
    /// without this the column names in them are unverified. Pins the four
    /// documented behaviours: archived sets are counted per root (unzipped
    /// sets and other roots' operations excluded), zips are summed DISTINCT, a
    /// recorded-but-missing zip contributes 0, and a calibration-originals
    /// operation (no `frames_set` link) contributes to NEITHER field — review
    /// fix Important-2, which is what keeps the two numbers describing the same
    /// thing instead of reading "0 sets · 4.2 GB".
    #[test]
    fn overview_counts_archived_sets_and_distinct_zip_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let arc = tmp.path().join("archive");
        std::fs::create_dir_all(&arc).unwrap();
        let arc = arc.canonicalize().unwrap().to_string_lossy().to_string();
        let other = tmp.path().join("archive_other");
        std::fs::create_dir_all(&other).unwrap();
        let other = other.canonicalize().unwrap().to_string_lossy().to_string();

        // Two real zips (10 + 20 bytes) plus one recorded path that is gone,
        // and a 40-byte zip belonging to a calibration-originals operation.
        let zip_a = format!("{arc}/lights.zip");
        let zip_b = format!("{arc}/flats.zip");
        let zip_gone = format!("{arc}/vanished.zip");
        let zip_cal = format!("{arc}/calibration_originals.zip");
        std::fs::write(&zip_a, [0u8; 10]).unwrap();
        std::fs::write(&zip_b, [0u8; 20]).unwrap();
        std::fs::write(&zip_cal, [0u8; 40]).unwrap();

        {
            let db = ctx.db.get().unwrap();
            let conn = db.conn();
            for path in [&arc, &other] {
                conn.execute(
                    "INSERT INTO archive_roots (path) VALUES (?1)",
                    rusqlite::params![path],
                )
                .unwrap();
            }
            let op_id = |root: &str| -> i64 {
                conn.execute(
                    "INSERT INTO archive_operations (archive_root_path, compression, status, started_at)
                     VALUES (?1, 'deflate', 'completed', '2026-01-01T00:00:00Z')",
                    rusqlite::params![root],
                )
                .unwrap();
                conn.last_insert_rowid()
            };
            let op = op_id(&arc);
            let op_other = op_id(&other);

            // Phase-2 calibration-originals archive under `arc`: a real
            // operation with a `calibration_set_id` subject and NO `frames_set`
            // row pointing at it.
            conn.execute(
                "INSERT INTO calibration_set (imagetyp, date) VALUES ('DARK', '2026-01-01')",
                [],
            )
            .unwrap();
            let cal_set = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO archive_operations
                    (calibration_set_id, archive_root_path, compression, status, started_at)
                 VALUES (?1, ?2, 'deflate', 'completed', '2026-01-05T00:00:00Z')",
                rusqlite::params![cal_set, arc],
            )
            .unwrap();
            let op_cal = conn.last_insert_rowid();

            // Two zipped sets under `arc`, one unzipped (archived_at NULL) that
            // must not count, and one zipped set under the other root.
            for (name, archived_at, operation) in [
                ("set-a", Some("2026-01-02T00:00:00Z"), op),
                ("set-b", Some("2026-01-03T00:00:00Z"), op),
                ("set-staged", None, op),
                ("set-other", Some("2026-01-04T00:00:00Z"), op_other),
            ] {
                conn.execute(
                    "INSERT INTO frames_set (name, archived_at, archive_operation_id) VALUES (?1, ?2, ?3)",
                    rusqlite::params![name, archived_at, operation],
                )
                .unwrap();
            }

            // Three files across two zips (so DISTINCT matters) + one row
            // pointing at the missing zip; one file under the other root's
            // operation; one under the calibration-originals operation.
            for (operation, zip, name) in [
                (op, &zip_a, "a.fits"),
                (op, &zip_a, "b.fits"),
                (op, &zip_b, "c.fits"),
                (op, &zip_gone, "d.fits"),
                (op_other, &zip_a, "e.fits"),
                (op_cal, &zip_cal, "dark.fits"),
            ] {
                conn.execute(
                    "INSERT INTO archive_operation_files
                        (operation_id, source_path, target_zip_path, target_path_in_zip,
                         expected_hash, disposition, frame_role, file_size_bytes)
                     VALUES (?1, ?2, ?3, ?4, 'deadbeef', 'move', 'light', 1)",
                    rusqlite::params![operation, format!("/src/{name}"), zip, name],
                )
                .unwrap();
            }
        }

        let ov = get_folder_overview(&ctx).unwrap();
        assert_eq!(ov.archive_roots.len(), 2);
        let a = ov.archive_roots.iter().find(|r| r.path == arc).unwrap();
        assert_eq!(a.set_count, 2);
        // 10 + 20 + 0 for the vanished zip; zip_a counted once, not twice, and
        // the 40-byte calibration-originals zip excluded with its operation.
        assert_eq!(
            a.total_zip_bytes, 30,
            "calibration-originals zip must be excluded, like its set is"
        );
        let o = ov.archive_roots.iter().find(|r| r.path == other).unwrap();
        assert_eq!(o.set_count, 1);
        assert_eq!(o.total_zip_bytes, 10);
    }
}

#[cfg(test)]
mod calibration_rebuild_tests {
    use super::*;

    #[test]
    fn calibration_frame_query_excludes_sibling_and_case_variant_roots() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let mk = |path: &str, imagetyp: &str| {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'd.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![path],
            ).unwrap();
            let fid = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp) VALUES (?1, ?2)",
                rusqlite::params![fid, imagetyp],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let mine = mk(r"C:\Astro\dark1.fits", "Dark");
        let sibling = mk(r"C:\Astro_backup\dark2.fits", "Dark");
        let case_variant = mk(r"C:\astro\dark3.fits", "Dark");

        let rows = calibration_frame_rows_under_root(&conn, r"C:\Astro").unwrap();
        let ids: Vec<i64> = rows.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&mine));
        assert!(
            !ids.contains(&sibling),
            "name-prefix sibling root leaked in"
        );
        assert!(
            !ids.contains(&case_variant),
            "case-variant root leaked in (LIKE was case-insensitive)"
        );
    }
}
