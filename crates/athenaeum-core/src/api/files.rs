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
    /// Zip files directly in the directory that hold cataloged frames. A zip is one item in the browser;
    /// `list_zip_entries` opens it.
    pub zips: Vec<crate::zip_store::catalog::ZipListItem>,
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

    // Auto-publish trigger (collab v3 wave 2, Task 10): resolved BEFORE the
    // cascade runs, since the cascade (`db::bulk_update_frame_metadata`)
    // can sever the very calibration/session junctions this join walks —
    // the frame sets these frames belonged to before the edit are exactly
    // the ones whose gate eligibility might have just changed.
    #[cfg(all(feature = "render", feature = "solver"))]
    let affected_sets = match crate::db::calibration_links::frame_set_ids_for_frames(
        &conn, &frame_ids,
    ) {
        Ok(set_ids) => set_ids,
        Err(error) => {
            tracing::warn!(%error, "auto-publish: failed to resolve frame sets for bulk metadata edit");
            Vec::new()
        }
    };

    let updated = crate::db::bulk_update_frame_metadata(&conn, &frame_ids, &edits)?;

    #[cfg(all(feature = "render", feature = "solver"))]
    if updated > 0 {
        crate::api::collab_autopublish::request_auto_publish_for_sets(&affected_sets);
    }

    Ok(updated)
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

    // Content mode is a single explicit key over every file. Otherwise the
    // view is Header (raw sub-frames, decided by their stored header) plus
    // Master (everything else, decided by a full-file hash) — the two
    // eligibility clauses are complements over the CLASSIFIED files, so no
    // file is decided twice (an unclassified `imagetyp IS NULL` frame is
    // deliberately in neither — see `DuplicateKey::eligibility`).
    let keys: &[crate::db::DuplicateKey] = if ctx
        .settings
        .get_duplicates_use_content_hash(&conn)
        .unwrap_or(false)
    {
        &[crate::db::DuplicateKey::Content]
    } else {
        &[
            crate::db::DuplicateKey::Header,
            crate::db::DuplicateKey::Master,
        ]
    };

    let mut all = Vec::new();
    for &key in keys {
        let groups = if crate::db::has_duplicate_cache(&conn, key).unwrap_or(false) {
            crate::db::get_cached_duplicates(&conn, key)?
        } else {
            crate::db::find_duplicate_groups(&conn, key)?
        };
        all.extend(groups);
    }
    all.sort_by(|a, b| b.file_count.cmp(&a.file_count).then(b.size.cmp(&a.size)));
    Ok(all)
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

    let zips = crate::zip_store::catalog::zips_in_directory(&conn, &directory_path, None).map_err(|e| {
        tracing::error!(path = %directory_path, error = %e, "zip listing failed");
        ApiError::Internal(format!("Could not list zips: {e}"))
    })?;

    Ok(DirectoryContents {
        subdirectories,
        files: files_in_dir,
        zips,
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

/// Resolve a requested longest edge to the render step it selects, plus that
/// step's stable name (M4d Task 3, fix round 1). The name is the SAME three
/// words `Resolution::from_string` parses, and it is what callers key a cache
/// file on: the render has only three behaviours, so keying on the raw
/// `max_px` would mint a distinct cache entry for every integer that resolves
/// to the same picture.
#[cfg(feature = "render")]
pub fn preview_step(max_px: u32) -> (crate::rustafits_processor::Resolution, &'static str) {
    use crate::rustafits_processor::Resolution;
    if max_px <= 512 {
        (Resolution::Thumbnail, "thumbnail")
    } else if max_px <= 2048 {
        (Resolution::Preview, "preview")
    } else {
        (Resolution::Full, "full")
    }
}

/// Render any FITS or XISF file on disk to JPEG bytes (M4d Task 3, ruling
/// R-M4d-5) — the SAME format-agnostic path `get_frame_preview` uses for a
/// catalog frame on both hosts, lifted out so a caller that has a file path
/// rather than a `frames` row (the stacking pipeline's written masters,
/// which are deliberately never cataloged as frames) renders identically.
///
/// `max_px` is the caller's requested longest edge; [`preview_step`] turns it
/// into one of `Resolution`'s three fixed steps, which are downscale FACTORS,
/// not target sizes:
/// - `<= 512` → `Thumbnail` (native / 4, JPEG quality 70),
/// - `<= 2048` → `Preview` (2x2 binning, i.e. native / 2, quality 85),
/// - otherwise → `Full` (native resolution, quality 95; a CFA frame is
///   VNG-debayered at native resolution there).
///
/// So the returned image is never SMALLER than `max_px` asked for — it is a
/// floor on the detail rendered, not an exact output size. The caller scales
/// the result down for display (an `<img>` does this for free).
///
/// Runs synchronously on the calling thread, on `pool` — the app's bounded
/// image pool — never the global rayon pool. The Tauri host wraps the call
/// in `block_in_place` and the web host in `spawn_blocking`, exactly as they
/// already do for `get_frame_preview`.
#[cfg(feature = "render")]
pub fn render_preview_from_path(
    path: &Path,
    max_px: u32,
    pool: &Arc<rayon::ThreadPool>,
) -> Result<Vec<u8>, ApiError> {
    use crate::rustafits_processor::process_fits_to_jpeg;

    let (resolution, _) = preview_step(max_px);

    if !path.exists() {
        return Err(ApiError::NotFound(format!(
            "file not found: {}",
            path.display()
        )));
    }

    let rendered = process_fits_to_jpeg(path, resolution, None, pool).map_err(|e| {
        tracing::error!(
            path = %path.display(),
            error = %format!("{e:#}"),
            "preview render failed"
        );
        ApiError::Internal(format!("failed to render {}: {e:#}", path.display()))
    })?;
    Ok(rendered.image_data)
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

    let zips = crate::zip_store::catalog::zips_in_directory(&conn, &directory_path, Some(instrume.as_str())).map_err(|e| {
        tracing::error!(path = %directory_path, error = %e, "zip listing failed");
        ApiError::Internal(format!("Could not list zips: {e}"))
    })?;

    Ok(DirectoryContents {
        subdirectories,
        files: files_in_dir,
        zips,
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
    let old_str = old.to_string_lossy().to_string();
    let new_str = new.to_string_lossy().to_string();
    // A zip's catalog row at the new name (a zip deleted outside the app, never rescanned) would make the
    // hot-sync fail after the file was already renamed; refuse before touching the disk.
    if !is_dir && crate::zip_store::catalog::is_known_zip(&conn, &old_str).map_err(zip_catalog_err)? {
        let taken = !is_same_file_case_variant(&old, &new)
            && crate::zip_store::catalog::is_known_zip(&conn, &new_str).map_err(zip_catalog_err)?;
        if taken {
            return Err(ApiError::Conflict(format!(
                "the catalog already holds a zip named {}",
                new.display()
            )));
        }
    }
    std::fs::rename(&old, &new).map_err(|e| {
        tracing::error!(src = %old.display(), dest = %new.display(), error = %e, "rename failed");
        ApiError::Internal(e.to_string())
    })?;

    // Hot-sync catalog rows. Fatal on failure — see RULE-2 FLAG doc above. One transaction, so a zip's rows
    // and its frames' virtual paths never end up half renamed.
    let tx = conn.unchecked_transaction()?;
    if is_dir {
        // Recursively rewire every catalog row under the renamed folder via
        // the SUBSTR-based prefix swap (REPLACE is unsafe — see
        // `rename_files_path_prefix` doc).
        let (prefix_old, prefix_new) = dir_rename_prefixes(&old_str, &new_str);
        let updated = crate::db::rename_files_path_prefix(&tx, &prefix_old, &prefix_new)?;
        // The swap above already moved the virtual paths of the frames inside zips; the zips' own rows and
        // markers follow here, and a virtual path is rewritten only where it still has the old spelling.
        let zips = crate::zip_store::catalog::rename_zip_prefix(&tx, &prefix_old, &prefix_new).map_err(zip_catalog_err)?;
        tracing::info!(count = updated, zips, src = %old_str, path = %new_str, "rename hot-synced catalog rows under directory");
    } else {
        let updated = tx.execute(
            "UPDATE files SET path = ?1, filename = ?2 WHERE path = ?3",
            rusqlite::params![&new_str, &new_name, &old_str],
        )?;
        let zips = crate::zip_store::catalog::rename_zip(&tx, &old_str, &new_str).map_err(zip_catalog_err)?;
        tracing::info!(count = updated, zips, src = %old_str, path = %new_str, "rename hot-synced catalog rows");
    }
    tx.commit()?;
    Ok(())
}

/// A zip bookkeeping failure at an API boundary: logged, then surfaced.
fn zip_catalog_err(e: anyhow::Error) -> ApiError {
    tracing::error!(error = %e, "zip catalog update failed");
    ApiError::Internal(format!("Zip catalog update failed: {e}"))
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
        let (_dir, root) = crate::test_support::canonical_tempdir();
        let policy = PathPolicy::AllowedRoots(vec![root.clone()]);
        // escape attempt via ..
        let evil = root.join("..").join("evil-dir");
        let r = resolve_mkdir_target(&evil.to_string_lossy(), &policy);
        assert!(r.is_err(), "traversal must be rejected");
        assert!(!root.parent().unwrap().join("evil-dir").exists());
    }

    #[test]
    fn mkdir_allows_legit_path_under_allowed_root() {
        let (_dir, root) = crate::test_support::canonical_tempdir();
        let policy = PathPolicy::AllowedRoots(vec![root.clone()]);
        let target = root.join("new-subdir");
        let r = resolve_mkdir_target(&target.to_string_lossy(), &policy).unwrap();
        // `resolve_mkdir_target` builds its return from a raw `canonicalize()`
        // of the parent (see its doc comment) — never normalized. On Windows
        // that is a `\\?\` verbatim path, so normalize before comparing
        // against `root`, which IS normalized (`canonical_tempdir()`).
        assert_eq!(
            crate::api::scan_roots::normalize_path(&r),
            root.join("new-subdir")
        );
    }

    #[test]
    fn mkdir_rejects_dotdot_final_component() {
        let (_dir, root) = crate::test_support::canonical_tempdir();
        let policy = PathPolicy::AllowedRoots(vec![root.clone()]);
        let evil = format!("{}/..", root.display());
        assert!(resolve_mkdir_target(&evil, &policy).is_err());
    }

    #[test]
    fn mkdir_fails_closed_when_parent_does_not_exist() {
        let (_dir, root) = crate::test_support::canonical_tempdir();
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

/// Verify two catalog files are byte-identical, banking the work.
///
/// The deep-verify button's backend. Three-tier economy:
///
/// 1. **Stored-hash shortcut** — when BOTH rows carry a non-empty
///    `files.strong_hash` and BOTH files still match their rows'
///    `(size, modified_at)` on disk ([`crate::duplicates::backfill::disk_matches_row`],
///    the same staleness contract as the master-hash pass), the verdict is
///    the hash comparison: zero bytes read. Inequality is sound (equal bytes
///    cannot hash differently); equality carries the same 64-bit xxh3 trust
///    the Master duplicate key already stakes deletions on.
/// 2. **Byte compare with free hashing** — otherwise both files are read in
///    lockstep and compared byte-for-byte
///    ([`crate::duplicates::verify_byte_identical_hashing`]). An identical
///    pair yields the full-content digest the read already paid for, which
///    is banked into `files.strong_hash` for every row still current on
///    disk — so the NEXT verify of this pair, and the master-hash pass, and
///    any future decide-by-bytes consumer, never pay this read again.
/// 3. **Mismatch stores nothing** — the compare early-exits, so no honest
///    full-content digest exists.
pub fn verify_duplicate_pair(
    ctx: &ServiceContext,
    file_a: i64,
    file_b: i64,
) -> Result<crate::duplicates::VerifyPairResult, ApiError> {
    use crate::duplicates::{VerifyMethod, VerifyPairResult};

    let db = db(ctx)?;
    let ((path_a, size_a, mtime_a, hash_a), (path_b, size_b, mtime_b, hash_b)) = {
        let conn = db.conn();
        let load = |id: i64| -> Result<(String, i64, String, Option<String>), ApiError> {
            conn.query_row(
                "SELECT path, size, modified_at, strong_hash FROM files WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(|e| {
                tracing::error!(file_id = id, error = %e, "verify pair: file row not found");
                ApiError::from(anyhow::anyhow!("file {id} not found: {e}"))
            })
        };
        (load(file_a)?, load(file_b)?)
    };

    let a_current = crate::duplicates::backfill::disk_matches_row(
        std::path::Path::new(&path_a),
        size_a,
        &mtime_a,
    );
    let b_current = crate::duplicates::backfill::disk_matches_row(
        std::path::Path::new(&path_b),
        size_b,
        &mtime_b,
    );

    let usable = |h: &Option<String>| h.as_deref().filter(|s| !s.is_empty()).map(str::to_owned);
    if a_current && b_current {
        if let (Some(ha), Some(hb)) = (usable(&hash_a), usable(&hash_b)) {
            tracing::debug!(
                file_a,
                file_b,
                identical = (ha == hb),
                "verify pair: decided from stored hashes"
            );
            return Ok(VerifyPairResult {
                identical: ha == hb,
                method: VerifyMethod::StoredHash,
            });
        }
    }

    // A zip-resident copy is compared through its extracted cache file; the pins live to the end of the compare.
    // A zip-resident row is never "current" (no file at its catalog path), so the compare never banks into it
    // (the extraction itself banks the hash it computed, `zip_store::materialize`).
    let paths = [path_a.clone(), path_b.clone()];
    let materialized = crate::zip_store::materialize::materialize_paths(
        ctx,
        &paths,
        &crate::events::NullEmitter,
        "",
        &std::sync::atomic::AtomicBool::new(false),
    )
    .map_err(|e| {
        tracing::error!(file_a, file_b, error = %e, "verify pair: a copy is not readable");
        ApiError::from(e)
    })?;
    let compared = crate::duplicates::verify_byte_identical_hashing(
        materialized.local(&path_a),
        materialized.local(&path_b),
    );
    drop(materialized);
    let (identical, digest) = compared.map_err(|e| {
        tracing::error!(file_a, file_b, error = %e, "verify pair: byte compare failed");
        ApiError::from(e)
    })?;

    if let (true, Some(d)) = (identical, digest) {
        // Bank the digest — but only into rows the disk still vouches for
        // (checked BEFORE the read, same stance as the master-hash pass); a
        // stale row is the scanner's to refresh, not ours to overwrite.
        let conn = db.conn();
        for (id, current) in [(file_a, a_current), (file_b, b_current)] {
            if !current {
                continue;
            }
            if let Err(e) = crate::db::bank_strong_hash(&conn, id, &d) {
                // The verdict stands either way — losing the banked hash
                // costs a re-read later, never a wrong answer now.
                tracing::error!(file_id = id, error = %e, "verify pair: strong_hash write failed");
            }
        }
        tracing::debug!(file_a, file_b, "verify pair: identical, hashes banked");
    }

    Ok(VerifyPairResult {
        identical,
        method: VerifyMethod::Bytes,
    })
}

#[cfg(test)]
mod verify_pair_tests {
    use crate::duplicates::VerifyMethod;
    use crate::package::xxh3_full_file;
    use crate::services::ServiceContext;
    use std::io::Write as _;

    /// files + frames rows for one on-disk file, carrying its REAL
    /// (size, modified_at) so the staleness guard sees a current row.
    fn seed_file(conn: &rusqlite::Connection, id: i64, path: &std::path::Path) {
        let meta = std::fs::metadata(path).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, ?4, ?5, 'FITS')",
            rusqlite::params![
                id,
                path.to_str().unwrap(),
                path.file_name().unwrap().to_str().unwrap(),
                meta.len() as i64,
                chrono::DateTime::<chrono::Utc>::from(meta.modified().unwrap()).to_rfc3339(),
            ],
        )
        .unwrap();
    }

    fn stored_hash(conn: &rusqlite::Connection, id: i64) -> Option<String> {
        conn.query_row("SELECT strong_hash FROM files WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
    }

    fn setup(
        bodies: &[(&str, &[u8])],
    ) -> (tempfile::TempDir, ServiceContext, Vec<std::path::PathBuf>) {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let mut paths = Vec::new();
        {
            let db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
            let conn = db.conn();
            for (i, (name, body)) in bodies.iter().enumerate() {
                let p = tmp.path().join(name);
                std::fs::File::create(&p).unwrap().write_all(body).unwrap();
                seed_file(&conn, (i + 1) as i64, &p);
                paths.push(p);
            }
        }
        (tmp, ctx, paths)
    }

    /// First verify of an identical pair pays the byte compare — and banks
    /// the full-content hash into BOTH rows, in `package::xxh3_full_file`'s
    /// format, so the read is never paid twice.
    #[test]
    fn identical_pair_stores_the_hash_for_both_files() {
        let body = vec![0x42u8; 30_000];
        let (tmp, ctx, paths) = setup(&[("a.fits", &body), ("b.fits", &body)]);

        let r = super::verify_duplicate_pair(&ctx, 1, 2).unwrap();
        assert!(r.identical);
        assert_eq!(r.method, VerifyMethod::Bytes);

        let db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        let conn = db.conn();
        let expect = xxh3_full_file(&paths[0]).unwrap();
        assert_eq!(stored_hash(&conn, 1).as_deref(), Some(expect.as_str()));
        assert_eq!(stored_hash(&conn, 2).as_deref(), Some(expect.as_str()));
    }

    /// Second verify of the same pair never reads the files: both rows carry
    /// a current stored hash, and equality is decided from the column.
    #[test]
    fn second_verify_decides_from_the_stored_hashes() {
        let body = vec![0x42u8; 30_000];
        let (_tmp, ctx, _paths) = setup(&[("a.fits", &body), ("b.fits", &body)]);

        assert_eq!(
            super::verify_duplicate_pair(&ctx, 1, 2).unwrap().method,
            VerifyMethod::Bytes
        );
        let r = super::verify_duplicate_pair(&ctx, 1, 2).unwrap();
        assert!(r.identical);
        assert_eq!(r.method, VerifyMethod::StoredHash);
    }

    /// Differing stored hashes on current rows prove the bytes differ —
    /// hash inequality is sound (equal bytes cannot hash differently), so
    /// the mismatch verdict needs no read either.
    #[test]
    fn differing_stored_hashes_decide_mismatch_without_reading() {
        let (tmp, ctx, _paths) =
            setup(&[("a.fits", &vec![1u8; 9_000]), ("b.fits", &vec![2u8; 9_000])]);
        {
            let db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
            let conn = db.conn();
            conn.execute("UPDATE files SET strong_hash = 'aaaa' WHERE id = 1", [])
                .unwrap();
            conn.execute("UPDATE files SET strong_hash = 'bbbb' WHERE id = 2", [])
                .unwrap();
        }

        let r = super::verify_duplicate_pair(&ctx, 1, 2).unwrap();
        assert!(!r.identical);
        assert_eq!(r.method, VerifyMethod::StoredHash);
    }

    /// A stale catalog row (its recorded size no longer matches the disk)
    /// disqualifies the shortcut AND the write-back for that row: the bytes
    /// are compared, the current row banks the hash, the stale row is left
    /// for the scanner to refresh.
    #[test]
    fn a_stale_row_falls_back_to_bytes_and_is_not_written() {
        let body = vec![0x42u8; 30_000];
        let (tmp, ctx, _paths) = setup(&[("a.fits", &body), ("b.fits", &body)]);
        {
            let db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
            let conn = db.conn();
            // Row 1 lies about its size — and carries a hash that would win
            // the shortcut if staleness were ignored.
            conn.execute(
                "UPDATE files SET size = 1, strong_hash = 'stale' WHERE id = 1",
                [],
            )
            .unwrap();
            conn.execute("UPDATE files SET strong_hash = 'stale' WHERE id = 2", [])
                .unwrap();
        }

        let r = super::verify_duplicate_pair(&ctx, 1, 2).unwrap();
        assert!(r.identical);
        assert_eq!(
            r.method,
            VerifyMethod::Bytes,
            "stale row must disqualify the shortcut"
        );

        let db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        let conn = db.conn();
        assert_eq!(
            stored_hash(&conn, 1).as_deref(),
            Some("stale"),
            "stale row must not be rewritten"
        );
        assert_ne!(
            stored_hash(&conn, 2).as_deref(),
            Some("stale"),
            "current row banks the fresh hash"
        );
    }

    /// A mismatching pair stores nothing: the compare early-exits, so no
    /// full-content digest exists to store.
    #[test]
    fn a_mismatching_pair_stores_nothing() {
        let (tmp, ctx, _paths) =
            setup(&[("a.fits", &vec![1u8; 9_000]), ("b.fits", &vec![2u8; 9_000])]);

        let r = super::verify_duplicate_pair(&ctx, 1, 2).unwrap();
        assert!(!r.identical);
        assert_eq!(r.method, VerifyMethod::Bytes);

        let db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        let conn = db.conn();
        assert_eq!(stored_hash(&conn, 1), None);
        assert_eq!(stored_hash(&conn, 2), None);
    }

    /// A disk file and a zip entry: `(ctx, disk file id, zip-resident file id)`.
    fn disk_and_zip_copies(disk_body: &[u8], zip_body: &[u8]) -> (tempfile::TempDir, ServiceContext, i64, i64) {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("astro")).unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let conn = ctx.db.get().unwrap().conn();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [tmp.path().join("astro").to_str().unwrap()]).unwrap();
        let disk = tmp.path().join("copy.fits");
        std::fs::write(&disk, disk_body).unwrap();
        seed_file(&conn, 1, &disk);
        let zip = crate::zip_store::scan::test_support::make_zip(
            &tmp.path().join("astro"),
            "night.zip",
            &[("Lights/copy.fits", zip_body.to_vec())],
            crate::archive::models::ArchiveCompression::Deflate,
        );
        crate::zip_store::scan::scan_zip(&conn, &zip, 1, false).unwrap();
        let zip_file: i64 = conn
            .query_row(
                "SELECT id FROM files WHERE path = ?1",
                [crate::zip_store::scan::virtual_path(&zip, "Lights/copy.fits")],
                |r| r.get(0),
            )
            .unwrap();
        drop(conn);
        (tmp, ctx, 1, zip_file)
    }

    #[test]
    fn verify_duplicate_pair_compares_disk_and_zip_copies() {
        let body = crate::zip_store::scan::test_support::light(7);
        let (_tmp, ctx, disk, zipped) = disk_and_zip_copies(&body, &body);
        let r = super::verify_duplicate_pair(&ctx, disk, zipped).unwrap();
        assert!(r.identical, "a byte-identical disk copy and zip entry verify");
        assert_eq!(r.method, VerifyMethod::Bytes);
        assert_eq!(ctx.zip_cache.extraction_count(), 1);

        let other = crate::zip_store::scan::test_support::light(8);
        let (_tmp2, ctx2, disk2, zipped2) = disk_and_zip_copies(&body, &other);
        let r = super::verify_duplicate_pair(&ctx2, disk2, zipped2).unwrap();
        assert!(!r.identical, "different bytes do not verify");
    }

    #[test]
    fn verify_duplicate_pair_fails_when_the_zip_is_not_reachable() {
        let body = crate::zip_store::scan::test_support::light(7);
        let (tmp, ctx, disk, zipped) = disk_and_zip_copies(&body, &body);
        std::fs::remove_file(tmp.path().join("astro").join("night.zip")).unwrap();
        let err = super::verify_duplicate_pair(&ctx, disk, zipped).unwrap_err().to_string();
        assert!(err.contains("not reachable"), "{err}");
    }
}

#[cfg(test)]
mod zip_whole_file_tests {
    use std::path::{Path, PathBuf, MAIN_SEPARATOR};

    use super::*;
    use crate::archive::models::ArchiveCompression;
    use crate::zip_store::presence::{row_is_present, ZipPresence};
    use crate::zip_store::scan::scan_zip;
    use crate::zip_store::scan::test_support::{light, make_zip};
    use crate::zip_store::test_support::{zip_files, Shape};

    /// A context whose catalog has one scan root, `<tmp>/astro`.
    fn fixture() -> (tempfile::TempDir, ServiceContext, PathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("astro");
        std::fs::create_dir_all(&root).unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        ctx.db
            .get()
            .unwrap()
            .conn()
            .execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [root.to_str().unwrap()])
            .unwrap();
        (tmp, ctx, root)
    }

    fn scan_into(ctx: &ServiceContext, dir: &Path, name: &str, seed: u32) -> PathBuf {
        let zip = make_zip(
            dir,
            name,
            &[("Lights/L_001.fits", light(seed)), ("Lights/L_002.fits", light(seed + 1))],
            ArchiveCompression::Store,
        );
        scan_zip(&ctx.db.get().unwrap().conn(), &zip, 1, false).unwrap();
        zip
    }

    /// `(path, archive_zip_path, archive_path_in_zip, present)` of every row, by path.
    fn rows(ctx: &ServiceContext) -> Vec<(String, Option<String>, Option<String>, bool)> {
        let database = ctx.db.get().unwrap();
        let conn = database.conn();
        let raw: Vec<(String, Option<String>, Option<String>)> = conn
            .prepare("SELECT path, archive_zip_path, archive_path_in_zip FROM files ORDER BY path")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let mut presence = ZipPresence::new();
        raw.into_iter()
            .map(|(p, z, e)| {
                let present = row_is_present(&mut presence, &p, z.as_deref(), e.as_deref());
                (p, z, e, present)
            })
            .collect()
    }

    #[test]
    fn directory_listing_shows_zips_with_frames_only() {
        let (_tmp, ctx, root) = fixture();
        let zip = scan_into(&ctx, &root, "night.zip", 1);
        // A zip holding no frame (a plain archive of notes) is not a catalog item.
        let notes = make_zip(&root, "notes.zip", &[("readme.txt", b"hi".to_vec())], ArchiveCompression::Store);
        scan_zip(&ctx.db.get().unwrap().conn(), &notes, 1, false).unwrap();
        let sub = root.join("sub");
        std::fs::create_dir(&sub).unwrap();
        scan_into(&ctx, &sub, "deeper.zip", 10);

        let top = get_directory_contents(&ctx, root.to_string_lossy().into_owned()).unwrap();
        assert_eq!(top.zips.len(), 1, "{:?}", top.zips);
        assert_eq!(top.zips[0].path, zip.to_string_lossy());
        assert_eq!(top.zips[0].frame_entries, 2);
        assert_eq!(top.zips[0].size, std::fs::metadata(&zip).unwrap().len() as i64);
        assert!(top.files.is_empty(), "the frames inside the zip are not entries of the folder");
        assert_eq!(top.subdirectories, vec![sub.to_string_lossy().into_owned()]);

        let inner = get_directory_contents(&ctx, sub.to_string_lossy().into_owned()).unwrap();
        assert_eq!(inner.zips.len(), 1);

        let by_camera = get_camera_directory_contents(
            &ctx,
            root.to_string_lossy().into_owned(),
            "C".into(),
            vec![sub.to_string_lossy().into_owned()],
        )
        .unwrap();
        assert_eq!(by_camera.zips.len(), 1);
        let other_camera = get_camera_directory_contents(&ctx, root.to_string_lossy().into_owned(), "Zzz".into(), vec![]).unwrap();
        assert!(other_camera.zips.is_empty());
    }

    #[test]
    fn renaming_a_zip_keeps_its_frames_present() {
        let (_tmp, ctx, root) = fixture();
        let zip = scan_into(&ctx, &root, "night.zip", 1);
        assert!(rows(&ctx).iter().all(|r| r.3));

        rename_path(&ctx, zip.to_string_lossy().into_owned(), "renamed.zip".into(), &PathPolicy::AllowAll).unwrap();

        let new_zip = root.join("renamed.zip").to_string_lossy().into_owned();
        let after = rows(&ctx);
        assert_eq!(after.len(), 2);
        for (path, zip_col, _entry, present) in &after {
            assert!(path.starts_with(&format!("{new_zip}{MAIN_SEPARATOR}")), "{path}");
            assert_eq!(zip_col.as_deref(), Some(new_zip.as_str()));
            assert!(present, "{path} must still be present");
        }
        let listed = get_directory_contents(&ctx, root.to_string_lossy().into_owned()).unwrap();
        assert_eq!(listed.zips.len(), 1);
        assert_eq!(listed.zips[0].path, new_zip);
    }

    #[test]
    fn renaming_a_zip_onto_a_catalog_row_without_a_file_is_refused_before_the_disk_moves() {
        let (_tmp, ctx, root) = fixture();
        let a = scan_into(&ctx, &root, "a.zip", 1);
        let b = scan_into(&ctx, &root, "b.zip", 10);
        std::fs::remove_file(&b).unwrap();
        let err = rename_path(&ctx, a.to_string_lossy().into_owned(), "b.zip".into(), &PathPolicy::AllowAll).unwrap_err();
        assert!(matches!(err, ApiError::Conflict(_)), "{err}");
        assert!(a.exists(), "the file was not renamed");
        assert!(rows(&ctx).iter().filter(|r| r.0.contains("a.zip")).all(|r| r.3));
    }

    #[test]
    fn renaming_a_folder_moves_foreign_and_our_zip_paths() {
        let (_tmp, ctx, root) = fixture();
        let old_dir = root.join("Old");
        std::fs::create_dir(&old_dir).unwrap();
        let foreign = scan_into(&ctx, &old_dir, "foreign.zip", 1);

        // One of our zips: a disk frame in Old/, zipped by Move and ZIP (the row keeps its original path).
        let original = old_dir.join("ours_L.fits");
        std::fs::write(&original, light(50)).unwrap();
        let ours = old_dir.join("ours.zip");
        {
            let database = ctx.db.get().unwrap();
            let conn = database.conn();
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (900, ?1, 'ours_L.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
                [original.to_str().unwrap()],
            )
            .unwrap();
            zip_files(&conn, &[900], &ours, Shape::Own { operation: 4 });
            conn.execute(
                "INSERT INTO zip_archives (path, size, modified_at, frame_entries, scanned_at)
                 VALUES (?1, ?2, 't', 1, 't')",
                rusqlite::params![ours.to_str().unwrap(), std::fs::metadata(&ours).unwrap().len() as i64],
            )
            .unwrap();
        }

        rename_path(&ctx, old_dir.to_string_lossy().into_owned(), "New".into(), &PathPolicy::AllowAll).unwrap();

        let new_dir = root.join("New");
        let new_foreign = new_dir.join("foreign.zip").to_string_lossy().into_owned();
        let new_ours = new_dir.join("ours.zip").to_string_lossy().into_owned();
        let after = rows(&ctx);
        let sep = MAIN_SEPARATOR;
        // Foreign rows: virtual path under the new zip, exactly once.
        for entry in ["L_001.fits", "L_002.fits"] {
            let want = format!("{new_foreign}{sep}Lights{sep}{entry}");
            let row = after.iter().find(|r| r.0 == want).unwrap_or_else(|| panic!("{want} missing in {after:?}"));
            assert_eq!(row.1.as_deref(), Some(new_foreign.as_str()));
            assert!(row.3);
        }
        // Our row: files.path follows the folder as before, archive_zip_path follows the zip.
        let want_path = new_dir.join("ours_L.fits").to_string_lossy().into_owned();
        let row = after.iter().find(|r| r.0 == want_path).unwrap_or_else(|| panic!("{want_path} missing in {after:?}"));
        assert_eq!(row.1.as_deref(), Some(new_ours.as_str()));
        assert!(row.3, "our row is present: its entry is in the renamed zip");
        assert_eq!(after.len(), 3, "no row doubled or lost: {after:?}");
        assert!(!after.iter().any(|r| r.0.contains("NewNew") || r.0.matches("foreign.zip").count() > 1), "{after:?}");

        let database = ctx.db.get().unwrap();
        let conn = database.conn();
        let conn_zips: Vec<String> = conn
            .prepare("SELECT path FROM zip_archives ORDER BY path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(conn_zips, vec![new_foreign, new_ours]);
        assert!(foreign.parent().unwrap().ends_with("Old"), "the fixture zip lived in Old");
    }
}
