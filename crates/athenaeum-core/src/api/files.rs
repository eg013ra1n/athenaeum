//! Shared `files` command-layer handlers — single business-logic source for
//! the Tauri (`commands/files.rs`) and web (`routes/files.rs`) wrappers. See
//! `.superpowers/sdd/p1-task-9-brief.md` for the conversion recipe this
//! module follows (reused verbatim by Tasks 10-12).
//!
//! Desktop bodies are authoritative; web-only divergences (path sandboxing,
//! HTTP-status-specific error classification) are folded in as `PathPolicy`
//! checks / `ApiError` variants per the recipe. See the Task 10 report for
//! the full drift catalog.
//!
//! Four of the 20 Tauri `files` commands are NOT single-sourced here — see
//! each handler's doc comment for why:
//! - `get_duplicates`, `get_frame_preview` — web equivalents live in other
//!   route files (`routes/duplicates.rs`, `routes/images.rs`), out of this
//!   task's declared 3-file scope (same pattern as Task 9's gap #7/#8).
//! - `get_database_path` — genuinely Tauri-only (`tauri::AppHandle::path()`);
//!   no `ServiceContext`-based equivalent exists. Left entirely in the Tauri
//!   wrapper.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::api::{db, ApiError, PathPolicy};
use crate::events::{emit_event, ProgressEmitter};
use crate::models::{DuplicateGroup, FileWithFrame, FrameMetadataEdits, MissingMetadataRow};
use crate::services::ServiceContext;

// ── Response DTOs (single-sourced; both wrapper crates import these) ────────

/// Mirrors the DirectoryContents DTO used by `get_directory_contents` and
/// `get_camera_directory_contents` on both platforms — was independently
/// duplicated (byte-identical) in `commands/files.rs` and `routes/files.rs`
/// pre-conversion.
#[derive(serde::Serialize)]
pub struct DirectoryContents {
    pub subdirectories: Vec<String>,
    pub files: Vec<FileWithFrame>,
}

/// One catalog search hit. Returns enough data for the UI to navigate the
/// active pane to the parent directory and highlight the file. Also
/// independently duplicated (byte-identical) pre-conversion.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSearchHit {
    pub file_id: i64,
    pub path: String,
    pub filename: String,
    pub object: Option<String>,
    pub filter: Option<String>,
    pub imagetyp: Option<String>,
    pub instrume: Option<String>,
    pub telescop: Option<String>,
    pub date_obs: Option<String>,
}

/// Web-only DTOs for `browse_directories` (no Tauri counterpart — desktop
/// uses a native folder-picker dialog instead). Moved here anyway per Task 9
/// precedent (`get_missing_files_counts`): in scope by file, single-sourced
/// for consistency even without a second platform to converge with.
#[derive(serde::Serialize)]
pub struct BrowseDirectoryEntry {
    pub name: String,
    pub path: String,
}

#[derive(serde::Serialize)]
pub struct BrowseDirectoriesResponse {
    pub current: String,
    pub parent: Option<String>,
    pub directories: Vec<BrowseDirectoryEntry>,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

pub fn get_files(
    ctx: &ServiceContext,
    limit: Option<usize>,
) -> Result<Vec<FileWithFrame>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let files = crate::db::get_files(&conn, limit)?;
    Ok(files
        .into_iter()
        .map(|(file, frame)| FileWithFrame { file, frame })
        .collect())
}

pub fn get_files_by_directory(
    ctx: &ServiceContext,
    directory_path: String,
    limit: Option<usize>,
) -> Result<Vec<FileWithFrame>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let files = crate::db::get_files_by_directory(&conn, &directory_path, limit)?;
    Ok(files
        .into_iter()
        .map(|(file, frame)| FileWithFrame { file, frame })
        .collect())
}

/// category: "all", "coordinates", "object", "datetime", "instrument", "frametype"
pub fn get_frames_with_missing_metadata(
    ctx: &ServiceContext,
    category: String,
) -> Result<Vec<MissingMetadataRow>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_frames_with_missing_metadata(
        &conn, &category,
    )?)
}

/// Bulk-update metadata fields (camera / date_obs / imagetyp / is_master) on
/// a set of frames. DB-only — the FITS files on disk are NOT rewritten.
/// Returns the number of rows updated.
pub fn bulk_update_frame_metadata(
    ctx: &ServiceContext,
    frame_ids: Vec<i64>,
    edits: FrameMetadataEdits,
) -> Result<usize, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::bulk_update_frame_metadata(
        &conn, &frame_ids, &edits,
    )?)
}

/// Re-decode the originally-scanned header values for the given frames out
/// of `fits_header` so the UI can show "what the file said before you
/// edited it" and offer per-field revert.
pub fn get_frame_metadata_originals(
    ctx: &ServiceContext,
    frame_ids: Vec<i64>,
) -> Result<Vec<crate::fits_parser::stored_header::FrameOriginalSnapshot>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_frame_metadata_originals(&conn, &frame_ids)?)
}

/// Aggregate a summary of which framesets / calibration sets the given
/// frames belong to (member of) or use (consumer of). Drives the metadata
/// pane's "Memberships" panel.
pub fn get_frame_memberships(
    ctx: &ServiceContext,
    frame_ids: Vec<i64>,
) -> Result<crate::db::FrameMembershipsSummary, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_frame_memberships_summary(&conn, &frame_ids)?)
}

/// Count the calibration-set / session relations the given frames currently
/// have. The dual-pane metadata editor calls this before applying edits so
/// the user can see how many links the cascade will sever.
pub fn count_frame_metadata_relations(
    ctx: &ServiceContext,
    frame_ids: Vec<i64>,
) -> Result<crate::db::FrameMetadataRelations, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::count_frame_metadata_relations(
        &conn, &frame_ids,
    )?)
}

/// Distinct non-empty INSTRUME values from the `frames` table, alphabetically
/// sorted. Feeds the Set Camera modal's existing-cameras list.
pub fn get_distinct_instrumes(ctx: &ServiceContext) -> Result<Vec<String>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_distinct_instrumes(&conn)?)
}

/// Duplicate file groups (from cache when warm, computed on the fly
/// otherwise).
///
/// NOT wired to the web wrapper: the web equivalent lives in
/// `routes/duplicates.rs`, not `routes/files.rs` — out of this task's
/// declared scope (same gap pattern as Task 9's `check_missing_files_in_scan_root`
/// / `set_scan_root_unique_camera_flag`). That file already carries a nearly
/// identical, independently-written implementation; only the Tauri wrapper
/// calls this handler for now.
pub fn get_duplicates(ctx: &ServiceContext) -> Result<Vec<DuplicateGroup>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let key = crate::db::DuplicateKey::from_setting(
        ctx.settings
            .get_duplicates_use_content_hash(&conn)
            .unwrap_or(false),
    );

    if crate::db::has_duplicate_cache(&conn, key).unwrap_or(false) {
        return Ok(crate::db::get_cached_duplicates(&conn, key)?);
    }
    Ok(crate::db::find_duplicate_groups(&conn, key)?)
}

/// Get directory contents (subdirectories and files).
pub fn get_directory_contents(
    ctx: &ServiceContext,
    directory_path: String,
) -> Result<DirectoryContents, ApiError> {
    use std::fs;

    let path = Path::new(&directory_path);

    // Use read_dir directly instead of an exists() pre-check. exists() returns
    // false on ANY error (permission flicker, transient FS state immediately
    // after a sibling rmdir, etc.), which would mislabel a still-present
    // directory as "doesn't exist". read_dir gives a real ErrorKind we can act on.
    let mut subdirectories = Vec::new();
    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ApiError::NotFound("Directory does not exist".to_string()));
        }
        Err(e) => {
            tracing::error!(path = %directory_path, error = %e, "directory read failed");
            return Err(ApiError::Internal(format!(
                "Could not read directory: {}",
                e
            )));
        }
    };

    for entry in entries {
        let entry = entry.map_err(|e| ApiError::Internal(e.to_string()))?;
        let entry_path = entry.path();
        let metadata = entry
            .metadata()
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        if metadata.is_dir() {
            subdirectories.push(entry_path.to_string_lossy().to_string());
        }
    }

    let db = db(ctx)?;
    let conn = db.conn();

    let db_files = crate::db::get_files_by_directory(&conn, &directory_path, None)?;
    let files_in_dir: Vec<FileWithFrame> = db_files
        .into_iter()
        .map(|(file, frame)| FileWithFrame { file, frame })
        .collect();

    subdirectories.sort();

    Ok(DirectoryContents {
        subdirectories,
        files: files_in_dir,
    })
}

/// Resolve the on-disk path for the file backing a frame, for
/// `get_frame_preview`.
///
/// Only the SQL lookup is single-sourced here — the rest of desktop's
/// `get_frame_preview` (delegating to `commands_rustafits::read_fits_image_rustafits`
/// for the actual FITS-to-JPEG render, then base64-encoding the result as a
/// `data:` URI) depends on a Tauri-only helper in a file outside this task's
/// scope (`commands_rustafits.rs`, itself a registered `#[tauri::command]`
/// taking `tauri::State<AppState>`) and produces a Tauri-only response shape
/// (web's `routes/images.rs` counterpart returns raw JPEG bytes with a
/// `Content-Type` header instead of a base64 data URI, and calls
/// `rustafits_processor::process_fits_to_jpeg` directly with a different
/// concurrency strategy — `spawn_blocking` vs desktop's `block_in_place`).
/// See the Task 10 report for the full reasoning.
///
/// NOTE for reviewers: web's `routes/images.rs::get_frame_preview` resolves
/// its `frame_id` argument against `files.id` directly (`SELECT path FROM
/// files WHERE id = ?1`), while this desktop-authoritative query resolves it
/// against `frames.id` (`JOIN frames fr ON f.id = fr.file_id WHERE fr.id =
/// ?1`) despite both using the same argument name. This is a pre-existing
/// divergence in an out-of-scope file, not something introduced or fixed by
/// this conversion — flagging it since it surfaced while cataloging drift.
pub fn get_frame_preview_path(ctx: &ServiceContext, frame_id: i64) -> Result<String, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    conn.query_row(
        "SELECT f.path FROM files f
         JOIN frames fr ON f.id = fr.file_id
         WHERE fr.id = ?1",
        [frame_id],
        |row| row.get(0),
    )
    .map_err(|e| ApiError::NotFound(format!("Failed to find frame {}: {}", frame_id, e)))
}

/// Get files with frames by frame IDs. Useful for loading full file data
/// when you only have frame IDs.
pub fn get_files_with_frames_by_ids(
    ctx: &ServiceContext,
    frame_ids: Vec<i64>,
) -> Result<Vec<FileWithFrame>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let frames = crate::db::get_frames_with_files_by_ids(&conn, &frame_ids)
        .map_err(|e| ApiError::Internal(format!("Failed to get frames: {}", e)))?;

    Ok(frames
        .into_iter()
        .map(|(_file_id, file, frame)| FileWithFrame {
            file,
            frame: Some(frame),
        })
        .collect())
}

/// Path to the log directory (rolling daily JSONL files live inside it).
///
/// No `ServiceContext` dependency — `crate::logging::get_path()` is pure
/// process-global state, identical to what both platforms already called
/// pre-conversion (desktop via `athenaeum_tauri::logging`, a bare
/// `pub use athenaeum_core::logging;` re-export; web's inline `mod.rs`
/// handler calls `athenaeum_core::logging::get_path()` directly). Desktop's
/// `Result<String,String>` (fatal when uninitialized) is authoritative here;
/// web's inline (out-of-scope, un-converted) handler returns `Json<Option<
/// String>>` instead — a real, disclosed divergence in error shape, not
/// touched since it lives in `routes/mod.rs`, outside this task's 3-file scope.
pub fn get_log_path() -> Result<String, ApiError> {
    crate::logging::get_path()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| ApiError::Internal("Logging not initialized".to_string()))
}

/// Get all distinct directory paths containing files for a given camera.
pub fn get_camera_directories(
    ctx: &ServiceContext,
    instrume: String,
) -> Result<Vec<String>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_camera_directories(&conn, &instrume)?)
}

/// Get directory contents filtered to only files from a specific camera.
pub fn get_camera_directory_contents(
    ctx: &ServiceContext,
    directory_path: String,
    instrume: String,
    camera_directories: Vec<String>,
) -> Result<DirectoryContents, ApiError> {
    use std::fs;

    let path = Path::new(&directory_path);

    let mut subdirectories = Vec::new();
    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ApiError::NotFound("Directory does not exist".to_string()));
        }
        Err(e) => {
            tracing::error!(path = %directory_path, error = %e, "directory read failed");
            return Err(ApiError::Internal(format!(
                "Could not read directory: {}",
                e
            )));
        }
    };

    for entry in entries {
        let entry = entry.map_err(|e| ApiError::Internal(e.to_string()))?;
        let entry_path = entry.path();
        let metadata = entry
            .metadata()
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        if metadata.is_dir() {
            let subdir_str = entry_path.to_string_lossy().to_string();
            // Include this subdir if any camera directory starts with it or equals it
            let is_relevant = camera_directories
                .iter()
                .any(|cam_dir| cam_dir.starts_with(&subdir_str));
            if is_relevant {
                subdirectories.push(subdir_str);
            }
        }
    }

    let db = db(ctx)?;
    let conn = db.conn();

    let db_files =
        crate::db::get_files_by_directory_for_camera(&conn, &directory_path, &instrume, None)?;
    let files_in_dir: Vec<FileWithFrame> = db_files
        .into_iter()
        .map(|(file, frame)| FileWithFrame { file, frame })
        .collect();

    subdirectories.sort();

    Ok(DirectoryContents {
        subdirectories,
        files: files_in_dir,
    })
}

// ============================================================================
// Dual-pane file browser (Phase 1: Move + catalog search)
// ============================================================================

/// Plan + immediately enqueue a Move operation. Returns the operation_id once
/// the plan is committed. The actual move runs on the global queue.
///
/// `ctx: &Arc<ServiceContext>` (not the base recipe's plain `&ServiceContext`)
/// because the queued job's `run` closure is `Box<dyn FnOnce() + Send +
/// 'static>` — it executes later, on the queue's worker thread, well after
/// this function returns, so it needs to own a clonable, 'static handle
/// rather than borrow one. Same for `emitter: E` (owned, not `&E` like
/// `start_scan_with_progress`/`check_missing_files_in_scan_root`): those
/// handlers use the emitter synchronously before returning, this one must
/// move it into the deferred closure.
///
/// Path sandboxing (web: `AllowedRoots`; desktop: `AllowAll` no-op) is a
/// rule-1 fold-in: pre-conversion desktop did no validation at all (enqueued
/// unconditionally); web validated `dest_dir` and every entry in `sources`
/// against `allowed_paths` first. Candidate paths are canonicalized with a
/// fallback to the raw path on failure, mirroring the pre-conversion
/// `path_inside_allowed` helper's behavior (`sources`/`dest_dir` are expected
/// to already exist, but a canonicalize() failure here shouldn't itself
/// become a new hard error where none existed before).
pub fn enqueue_move_operation<E: ProgressEmitter + 'static>(
    ctx: &Arc<ServiceContext>,
    sources: Vec<String>,
    dest_dir: String,
    policy: &PathPolicy,
    emitter: E,
) -> Result<i64, ApiError> {
    use crate::file_op::{db as fdb, executor as fexec, models::FileOpStatus, planner as fplan};
    use crate::services::operation_queue::{OperationKind, QueuedJob};
    use crate::services::ArchiveHandle;
    use std::sync::atomic::AtomicBool;

    let dest_candidate = Path::new(&dest_dir)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&dest_dir));
    policy.check(&dest_candidate)?;
    for s in &sources {
        let src_candidate = Path::new(s)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(s));
        policy.check(&src_candidate)?;
    }

    let db = db(ctx)?;
    let conn = db.conn();

    let source_paths: Vec<PathBuf> = sources.iter().map(PathBuf::from).collect();
    let plan = fplan::build_move_plan(&conn, source_paths, PathBuf::from(&dest_dir))
        .map_err(|e| ApiError::Invalid(format!("{:#}", e)))?;
    let op_id = plan.operation_id;

    // Cancel flag — stored in active_archives map until file_op gets its own
    // map (intentional reuse since the map is just "active op handles" and
    // already keyed by operation_id; the kind is recorded on the row).
    let cancel_flag = Arc::new(AtomicBool::new(false));

    // Registered BEFORE enqueueing (web's pre-conversion order, now used for
    // both platforms) so a `cancel` request can never race a same-instant
    // dequeue — desktop's pre-conversion order registered it after.
    {
        let mut map = ctx.active_archives.lock().unwrap();
        map.insert(
            op_id,
            ArchiveHandle {
                operation_id: op_id,
                cancel_flag: cancel_flag.clone(),
            },
        );
    }

    let ctx_for_worker = ctx.clone();
    let cancel_for_worker = cancel_flag.clone();
    ctx.operation_queue.enqueue(QueuedJob {
        kind: OperationKind::FileOpMove,
        operation_id: op_id,
        run: Box::new(move || {
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();

            // Heal any abandoned cross-volume moves left over from a prior
            // crash before running this one. Cheap no-op when there's
            // nothing to heal; errors here must not fail this user's move.
            match crate::file_op::reconcile::reconcile_abandoned_commit_moves(&conn) {
                Ok(summary) if summary.healed > 0 || !summary.skipped.is_empty() => {
                    emit_event(
                        &emitter,
                        "file-op-reconciled",
                        &crate::file_op::models::FileOpReconciled {
                            healed: summary.healed,
                            skipped: summary.skipped.len(),
                            operation_ids: summary.touched_operation_ids(),
                        },
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(operation_id = op_id, error = ?e, "file_op reconcile (pre-enqueue) failed"),
            }

            let result = fexec::run_operation(&conn, op_id, &cancel_for_worker, &emitter);
            let outcome = match result {
                Ok(()) => "completed",
                Err(e) => {
                    if fexec::was_cancelled(&e) {
                        let _ = fdb::update_operation_status(&conn, op_id, FileOpStatus::Cancelled, None);
                        "cancelled"
                    } else {
                        tracing::error!(operation_id = op_id, error = ?e, "file_op failed");
                        let msg = format!("{:#}", e);
                        let _ = fdb::update_operation_status(&conn, op_id, FileOpStatus::Failed, Some(&msg));
                        "failed"
                    }
                }
            };
            // Desktop-authoritative: pre-conversion web never emitted this
            // event at all, even though the shared frontend
            // (`DualPaneFileBrowser.tsx`) already listens for it
            // unconditionally on both platforms. Folding in desktop's
            // behavior here closes what looks like a latent web-only gap
            // rather than a deliberate lenient/fatal choice — flagged in the
            // Task 10 report rather than picked silently.
            emit_event(
                &emitter,
                "file-op-finished",
                &serde_json::json!({ "operation_id": op_id, "outcome": outcome, "kind": "move" }),
            );
        }),
    });

    Ok(op_id)
}

/// Free-text catalog search across all scan roots. Returns up to `limit`
/// hits matching by filename, OBJECT, FILTER, IMAGETYP, INSTRUME, or
/// TELESCOP (case-insensitive substring match).
///
/// `instrume_filter` constrains hits to a single camera (frames.instrume
/// equality). Used by the per-camera dual-pane file browser so a camera-
/// scoped view's search bar can't surface frames from other cameras.
pub fn search_catalog(
    ctx: &ServiceContext,
    query: String,
    limit: Option<u32>,
    instrume_filter: Option<String>,
) -> Result<Vec<CatalogSearchHit>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.unwrap_or(200).min(500) as i64;
    let pattern = format!("%{}%", trimmed.to_lowercase());

    // Two SQL branches so `instrume_filter` either pins fr.instrume or is
    // skipped entirely (no `OR instrume IS NULL` ambiguity, no wasted bind).
    // The Vec is bound to a local before the block returns so `stmt`
    // outlives the iterator (rustc E0597).
    let rows = if let Some(cam) = instrume_filter.as_deref() {
        let mut stmt = conn.prepare(
            "SELECT f.id, f.path, f.filename,
                    fr.object, fr.filter, fr.imagetyp, fr.instrume, fr.telescop, fr.date_obs
             FROM files f
             LEFT JOIN frames fr ON fr.file_id = f.id
             WHERE fr.instrume = ?3
               AND (LOWER(f.filename) LIKE ?1
                 OR LOWER(f.path) LIKE ?1
                 OR LOWER(IFNULL(fr.object,''))    LIKE ?1
                 OR LOWER(IFNULL(fr.filter,''))    LIKE ?1
                 OR LOWER(IFNULL(fr.imagetyp,''))  LIKE ?1
                 OR LOWER(IFNULL(fr.instrume,''))  LIKE ?1
                 OR LOWER(IFNULL(fr.telescop,''))  LIKE ?1)
             ORDER BY fr.date_obs DESC
             LIMIT ?2",
        )?;
        let hits: Vec<CatalogSearchHit> = stmt
            .query_map(rusqlite::params![pattern, limit, cam], map_search_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        hits
    } else {
        let mut stmt = conn.prepare(
            "SELECT f.id, f.path, f.filename,
                    fr.object, fr.filter, fr.imagetyp, fr.instrume, fr.telescop, fr.date_obs
             FROM files f
             LEFT JOIN frames fr ON fr.file_id = f.id
             WHERE LOWER(f.filename) LIKE ?1
                OR LOWER(f.path) LIKE ?1
                OR LOWER(IFNULL(fr.object,''))    LIKE ?1
                OR LOWER(IFNULL(fr.filter,''))    LIKE ?1
                OR LOWER(IFNULL(fr.imagetyp,''))  LIKE ?1
                OR LOWER(IFNULL(fr.instrume,''))  LIKE ?1
                OR LOWER(IFNULL(fr.telescop,''))  LIKE ?1
             ORDER BY fr.date_obs DESC
             LIMIT ?2",
        )?;
        let hits: Vec<CatalogSearchHit> = stmt
            .query_map(rusqlite::params![pattern, limit], map_search_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        hits
    };

    Ok(rows)
}

/// Map a set of selected browser paths (files and/or folders) to the catalog
/// frame ids they cover, so the dual-pane file browser (which selects by
/// path) can feed the Send dialog (which addresses frames by id). Folders
/// match recursively; overlapping file + parent-folder selections are
/// deduped; non-cataloged paths contribute nothing. Returns an ascending,
/// stable, DISTINCT list.
pub fn resolve_frame_ids_for_paths(
    ctx: &ServiceContext,
    paths: Vec<String>,
) -> Result<Vec<i64>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let ids = crate::db::frame_ids_under_paths(&conn, &paths)?;
    Ok(ids)
}

/// Row -> CatalogSearchHit mapper shared between the two SQL branches above.
fn map_search_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CatalogSearchHit> {
    Ok(CatalogSearchHit {
        file_id: row.get(0)?,
        path: row.get(1)?,
        filename: row.get(2)?,
        object: row.get(3)?,
        filter: row.get(4)?,
        imagetyp: row.get(5)?,
        instrume: row.get(6)?,
        telescop: row.get(7)?,
        date_obs: row.get(8)?,
    })
}

/// Resolve and validate an mkdir target. Fails closed instead of the naive
/// "canonicalize the target, falling back to the raw string on failure"
/// pattern used elsewhere in this file (`enqueue_move_operation`,
/// `rename_path`) for sources/dests that are expected to already exist: an
/// mkdir target never exists yet, so `canonicalize()` on it always fails,
/// and falling back to the raw string lets a traversal string like
/// `"/allowed/../../tmp/evil"` pass `PathPolicy::check` lexically
/// (`Path::starts_with` is component-prefix only, not resolution — see its
/// doc comment) even though it resolves outside the allowed root once
/// created.
///
/// Instead: canonicalize the PARENT (which does exist), fail closed
/// (`ApiError::Invalid`) if it's missing or fails to canonicalize, reject a
/// final path component that is empty, `.`, `..`, or itself contains a path
/// separator, then re-join that final component onto the canonical parent.
/// `PathPolicy::check` runs against that canonicalized-parent+child path, so
/// traversal segments anywhere in the input are resolved against the real
/// filesystem before the containment check ever sees them — same fail-closed
/// posture as `browse_directories` (which requires the *target itself* to
/// canonicalize, since it must already exist to be browsed).
///
/// `pub(crate)` (not private) so it can be unit-tested directly without a
/// `ServiceContext` — the scan-root containment check in
/// `mkdir_in_scan_root` below needs the DB handle and stays there.
pub(crate) fn resolve_mkdir_target(path: &str, policy: &PathPolicy) -> Result<PathBuf, ApiError> {
    let target = PathBuf::from(path);

    let final_component = target.file_name().ok_or_else(|| {
        ApiError::Invalid(format!("'{}' has no valid final path component", path))
    })?;
    let final_str = final_component.to_string_lossy();
    if final_str.is_empty()
        || final_str == "."
        || final_str == ".."
        || final_str.contains('/')
        || final_str.contains('\\')
    {
        return Err(ApiError::Invalid(format!(
            "'{}' is not a valid directory name",
            path
        )));
    }

    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| ApiError::Invalid(format!("'{}' has no parent directory", path)))?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|e| ApiError::Invalid(format!("parent of '{}' does not exist: {}", path, e)))?;

    let candidate = canonical_parent.join(final_component);
    policy.check(&candidate)?;
    Ok(candidate)
}

/// Create a directory inside a scan root. Validated to refuse paths that
/// would land outside any configured scan root.
///
/// Path sandboxing (web: `AllowedRoots`; desktop: `AllowAll` no-op) is a
/// rule-1 fold-in, checked (via `resolve_mkdir_target`) before the scan-root
/// containment check below (matches pre-conversion web ordering).
pub fn mkdir_in_scan_root(
    ctx: &ServiceContext,
    path: String,
    policy: &PathPolicy,
) -> Result<(), ApiError> {
    let target = resolve_mkdir_target(&path, policy)?;

    let db = db(ctx)?;
    let conn = db.conn();

    let scan_roots = crate::db::get_scan_roots(&conn)?;
    let inside_root = scan_roots.iter().filter(|r| r.enabled).any(|r| {
        let root = PathBuf::from(&r.path);
        let rc = std::fs::canonicalize(&root).unwrap_or(root);
        target.starts_with(&rc)
    });
    if !inside_root {
        return Err(ApiError::Invalid(format!(
            "'{}' is not inside any scan root",
            path
        )));
    }
    std::fs::create_dir_all(&target).map_err(|e| {
        tracing::error!(path = %path, error = %e, "mkdir failed");
        ApiError::Internal(e.to_string())
    })
}

/// Directory-rename prefixes for `rename_files_path_prefix`, built with the
/// path's own native separator (the doc contract there requires a trailing
/// separator; hardcoding '/' silently matched zero rows on Windows).
///
/// Trailing separators are trimmed off the inputs first: a caller-supplied
/// `/data/Old/` would otherwise build the prefix `/data/Old//`, which matches
/// zero stored rows.
fn dir_rename_prefixes(old_str: &str, new_str: &str) -> (String, String) {
    let sep = crate::db::native_separator_of(old_str);
    let old = old_str.trim_end_matches(sep);
    let new = new_str.trim_end_matches(sep);
    (format!("{old}{sep}"), format!("{new}{sep}"))
}

/// True when `new` names the SAME on-disk file as `old` (case-insensitive
/// filesystems: renaming `m31.fits` → `M31.fits` makes `new.exists()` true
/// for the very file being renamed — that is not a collision).
///
/// Gated on the two file NAMES differing only by case: canonicalize alone
/// resolves symlinks and hardlink aliases, so a genuinely distinct
/// destination that merely POINTS at `old` (e.g. `link.fits` → `a.fits`)
/// would otherwise be waved through the collision check and overwritten.
pub(crate) fn is_same_file_case_variant(old: &Path, new: &Path) -> bool {
    old.file_name()
        .zip(new.file_name())
        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
        && new.exists()
        && old.exists()
        && matches!(
            (std::fs::canonicalize(old), std::fs::canonicalize(new)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// Rename a file or directory in place. Same-folder rename only (i.e., no
/// path traversal). Hot-syncs the catalog: for a file, updates `files.path`
/// and `files.filename`; for a directory, updates every `files.path` whose
/// path begins with the old directory path.
///
/// Path sandboxing (web: `AllowedRoots`; desktop: `AllowAll` no-op) is a
/// rule-1 fold-in, same canonicalize-with-fallback pattern as
/// `mkdir_in_scan_root` (checked against `old_path`, which does exist).
///
/// RULE-2 FLAG: the catalog hot-sync step (`rename_files_path_prefix` for a
/// directory, or the single-row `UPDATE` for a file) is FATAL on desktop
/// pre-conversion (`?`-propagated — an error here fails the whole command
/// even though the filesystem rename already succeeded) but was LENIENT on
/// web pre-conversion (`match { Ok => info!, Err => error! }`, always
/// returning success). Per the binding hard rule, desktop's fatal semantics
/// win here unconditionally: this merge makes web's hot-sync failure surface
/// as a 500 instead of a silently-swallowed success, matching desktop. Note
/// this does NOT change the underlying risk profile (on either platform, the
/// filesystem rename has already happened by the time this step runs, so a
/// hot-sync failure still leaves the catalog stale relative to disk either
/// way) — it only changes whether the caller is told about it.
///
/// `old_path` is separator-folded on entry (`db::normalize_separators`) so a
/// '/'-spelled Windows path — which the filesystem accepts — still matches the
/// '\'-spelled rows the catalog stores. A case-only rename
/// (`m31.fits` → `M31.fits`) is allowed rather than reported as a collision;
/// see `is_same_file_case_variant`.
pub fn rename_path(
    ctx: &ServiceContext,
    old_path: String,
    new_name: String,
    policy: &PathPolicy,
) -> Result<(), ApiError> {
    if new_name.contains('/') || new_name.contains('\\') || new_name.trim().is_empty() {
        return Err(ApiError::Invalid(
            "new name must be a single path component".into(),
        ));
    }

    let old_path = crate::db::normalize_separators(&old_path);

    let old = PathBuf::from(&old_path);

    let candidate = old.canonicalize().unwrap_or_else(|_| old.clone());
    policy.check(&candidate)?;

    let db = db(ctx)?;
    let conn = db.conn();

    let parent = old
        .parent()
        .ok_or_else(|| ApiError::Invalid("source has no parent directory".to_string()))?
        .to_path_buf();
    let new = parent.join(&new_name);
    if new.exists() && !is_same_file_case_variant(&old, &new) {
        return Err(ApiError::Conflict(format!(
            "target already exists: {}",
            new.display()
        )));
    }

    // Validate inside scan root.
    let scan_roots = crate::db::get_scan_roots(&conn)?;
    let inside_root = scan_roots.iter().filter(|r| r.enabled).any(|r| {
        let root = PathBuf::from(&r.path);
        let rc = std::fs::canonicalize(&root).unwrap_or(root);
        let oc = std::fs::canonicalize(&old).unwrap_or_else(|_| old.clone());
        oc.starts_with(&rc)
    });
    if !inside_root {
        return Err(ApiError::Invalid(format!(
            "'{}' is not inside any scan root",
            old_path
        )));
    }

    let is_dir = old.is_dir();
    std::fs::rename(&old, &new).map_err(|e| {
        tracing::error!(src = %old.display(), dest = %new.display(), error = %e, "rename failed");
        ApiError::Internal(e.to_string())
    })?;

    // Hot-sync catalog rows. Fatal on failure — see RULE-2 FLAG doc above.
    let old_str = old.to_string_lossy().to_string();
    let new_str = new.to_string_lossy().to_string();
    if is_dir {
        // Recursively rewire every catalog row under the renamed folder via
        // the SUBSTR-based prefix swap (REPLACE is unsafe — see
        // `rename_files_path_prefix` doc).
        let (prefix_old, prefix_new) = dir_rename_prefixes(&old_str, &new_str);
        let updated = crate::db::rename_files_path_prefix(&conn, &prefix_old, &prefix_new)?;
        tracing::info!(count = updated, src = %old_str, path = %new_str, "rename hot-synced catalog rows under directory");
    } else {
        let updated = conn.execute(
            "UPDATE files SET path = ?1, filename = ?2 WHERE path = ?3",
            rusqlite::params![&new_str, &new_name, &old_str],
        )?;
        tracing::info!(count = updated, src = %old_str, path = %new_str, "rename hot-synced catalog rows");
    }
    Ok(())
}

/// Web-only: no Tauri command by this name (desktop uses a native
/// folder-picker dialog instead). Converted anyway per Task 9 precedent
/// (`get_missing_files_counts`): in scope by file, single-sourced for
/// consistency.
///
/// No `ServiceContext` dependency — purely filesystem-based. Takes
/// `root_paths` pre-resolved by the caller because the "scan vs export
/// scope" selection depends on `WebAppState::allowed_paths` /
/// `WebAppState::export_dir`, which are web-only fields with no
/// `ServiceContext` equivalent (mirrors how `set_scan_root_monitor_enabled`
/// in Task 9 took an extra non-`ServiceContext` param for state that lives
/// alongside, not inside, the shared context).
pub fn browse_directories(
    path: Option<String>,
    root_paths: &[PathBuf],
) -> Result<BrowseDirectoriesResponse, ApiError> {
    let path_str = path.unwrap_or_default();

    // If no path provided, return root paths as top-level entries
    if path_str.is_empty() || path_str == "/" {
        let directories: Vec<BrowseDirectoryEntry> = root_paths
            .iter()
            .filter(|p| p.is_dir())
            .map(|p| {
                let s = p.to_string_lossy().to_string();
                BrowseDirectoryEntry {
                    name: p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| s.clone()),
                    path: s,
                }
            })
            .collect();

        return Ok(BrowseDirectoriesResponse {
            current: "/".to_string(),
            parent: None,
            directories,
        });
    }

    let target = PathBuf::from(&path_str);
    let canonical = crate::api::scan_roots::normalize_path(
        &target
            .canonicalize()
            .map_err(|e| ApiError::Invalid(format!("Invalid path: {}", e)))?,
    );

    // Security: validate path is within scope roots (both sides normalized —
    // this is the one canonicalize in the domain whose result ESCAPES to the
    // frontend and comes back through add_scan_root / export_to_wbpp).
    let is_allowed = root_paths.iter().any(|allowed| {
        allowed
            .canonicalize()
            .map(|a| canonical.starts_with(crate::api::scan_roots::normalize_path(&a)))
            .unwrap_or(false)
    });

    if !is_allowed {
        return Err(ApiError::Forbidden(
            "Path is outside allowed directories".to_string(),
        ));
    }

    if !canonical.is_dir() {
        return Err(ApiError::Invalid("Path is not a directory".to_string()));
    }

    let mut directories = Vec::new();
    let entries = std::fs::read_dir(&canonical)
        .map_err(|e| ApiError::Internal(format!("Failed to read directory: {}", e)))?;

    for entry in entries {
        let entry = entry.map_err(|e| ApiError::Internal(e.to_string()))?;
        let metadata = entry
            .metadata()
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        if metadata.is_dir() {
            let entry_path = entry.path();
            directories.push(BrowseDirectoryEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                path: entry_path.to_string_lossy().to_string(),
            });
        }
    }
    directories.sort_by(|a, b| a.name.cmp(&b.name));

    let parent = canonical.parent().and_then(|p| {
        let parent_str = p.to_string_lossy().to_string();
        // Only return parent if it's still within a scope root
        let parent_within = root_paths.iter().any(|allowed| {
            allowed
                .canonicalize()
                .map(|a| p.starts_with(crate::api::scan_roots::normalize_path(&a)))
                .unwrap_or(false)
        });
        if parent_within {
            Some(parent_str)
        } else {
            // Parent is at or above a root — go back to root listing
            None
        }
    });

    Ok(BrowseDirectoriesResponse {
        current: canonical.to_string_lossy().to_string(),
        parent,
        directories,
    })
}

#[cfg(test)]
mod mkdir_target_tests {
    use super::*;
    use crate::api::PathPolicy;

    #[test]
    fn mkdir_rejects_parent_traversal_against_allowed_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let policy = PathPolicy::AllowedRoots(vec![root.clone()]);
        // escape attempt via ..
        let evil = root.join("..").join("evil-dir");
        let r = resolve_mkdir_target(&evil.to_string_lossy(), &policy);
        assert!(r.is_err(), "traversal must be rejected");
        assert!(!root.parent().unwrap().join("evil-dir").exists());
    }

    #[test]
    fn mkdir_allows_legit_path_under_allowed_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let policy = PathPolicy::AllowedRoots(vec![root.clone()]);
        let target = root.join("new-subdir");
        let r = resolve_mkdir_target(&target.to_string_lossy(), &policy).unwrap();
        assert_eq!(r, root.join("new-subdir"));
    }

    #[test]
    fn mkdir_rejects_dotdot_final_component() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let policy = PathPolicy::AllowedRoots(vec![root.clone()]);
        let evil = format!("{}/..", root.display());
        assert!(resolve_mkdir_target(&evil, &policy).is_err());
    }

    #[test]
    fn mkdir_fails_closed_when_parent_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let policy = PathPolicy::AllowAll;
        let target = root.join("nonexistent-parent").join("child");
        assert!(resolve_mkdir_target(&target.to_string_lossy(), &policy).is_err());
    }
}

#[cfg(test)]
mod dir_rename_prefix_tests {
    use super::*;

    #[test]
    fn dir_rename_prefixes_use_native_separator() {
        assert_eq!(
            dir_rename_prefixes(r"C:\data\Old", r"C:\data\New"),
            (r"C:\data\Old\".to_string(), r"C:\data\New\".to_string())
        );
        assert_eq!(
            dir_rename_prefixes("/data/Old", "/data/New"),
            ("/data/Old/".to_string(), "/data/New/".to_string())
        );
    }

    /// A caller-supplied trailing separator must not double up — `/data/Old//`
    /// as a prefix matches nothing.
    #[test]
    fn dir_rename_prefixes_trim_caller_supplied_trailing_separator() {
        assert_eq!(
            dir_rename_prefixes("/data/Old/", "/data/New/"),
            ("/data/Old/".to_string(), "/data/New/".to_string())
        );
        assert_eq!(
            dir_rename_prefixes(r"C:\data\Old\", r"C:\data\New\"),
            (r"C:\data\Old\".to_string(), r"C:\data\New\".to_string())
        );
    }

    #[test]
    fn case_variant_identity_check() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.fits");
        std::fs::write(&a, b"x").unwrap();
        let upper = dir.path().join("A.FITS");
        let volume_is_case_insensitive = upper.exists();
        assert_eq!(
            is_same_file_case_variant(&a, &upper),
            volume_is_case_insensitive
        );
        let b = dir.path().join("b.fits");
        std::fs::write(&b, b"y").unwrap();
        assert!(
            !is_same_file_case_variant(&a, &b),
            "distinct files are never the same"
        );

        // A symlink ALIASING the source canonicalizes to the same target, but
        // it is a real, distinct destination name — renaming onto it is a
        // collision, not a case-only rename.
        #[cfg(unix)]
        {
            let link = dir.path().join("link.fits");
            std::os::unix::fs::symlink(&a, &link).unwrap();
            assert!(
                !is_same_file_case_variant(&a, &link),
                "a symlink aliasing the source is not a case variant of it"
            );
        }
    }
}

#[cfg(test)]
mod duplicate_key_tests {
    use crate::services::ServiceContext;

    /// With the setting off, the handler must use the header key and find the
    /// pair whose mtimes drifted. With it on, it must use the content key and
    /// find nothing (no content index has been built), rather than silently
    /// falling back to the header key.
    #[test]
    fn get_duplicates_follows_the_use_content_hash_setting() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));

        {
            let db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
            let conn = db.conn();
            conn.execute(
                "INSERT INTO scan_roots (path, find_duplicates) VALUES ('/vol', 1)",
                [],
            )
            .unwrap();
            for (id, path, mtime) in [
                (1i64, "/vol/a/f.fits", "2024-10-05T04:21:46+00:00"),
                (2, "/vol/b/f.fits", "2024-10-05T04:21:44.307+00:00"),
            ] {
                conn.execute(
                    "INSERT INTO files (id, path, filename, size, modified_at, format)
                     VALUES (?1, ?2, 'f.fits', 100, ?3, 'FITS')",
                    rusqlite::params![id, path, mtime],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO frames (id, file_id, imagetyp, is_master)
                     VALUES (?1, ?1, 'Flat', 0)",
                    rusqlite::params![id],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO fits_header (file_id, header, header_fingerprint)
                     VALUES (?1, 'HDR', ?2)",
                    rusqlite::params![id, crate::fingerprint::compute_header_fingerprint("HDR")],
                )
                .unwrap();
            }
        }

        let groups = super::get_duplicates(&ctx).unwrap();
        assert_eq!(
            groups.len(),
            1,
            "header key is the default, got {groups:#?}"
        );
        assert_eq!(groups[0].file_count, 2);

        {
            let db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
            let conn = db.conn();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::DUPLICATES_USE_CONTENT_HASH,
                "true",
            )
            .unwrap();
        }

        let groups = super::get_duplicates(&ctx).unwrap();
        assert!(
            groups.is_empty(),
            "content key with no content index must find nothing, got {groups:#?}"
        );
    }
}
