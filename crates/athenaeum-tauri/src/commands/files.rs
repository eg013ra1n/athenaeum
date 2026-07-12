// File commands - file operations and browsing
//
// Thin wrappers only: extraction + policy construction + handler call + error
// mapping. Business logic lives in `athenaeum_core::api::files` (Task 10
// conversion — see `.superpowers/sdd/p1-task-10-report.md`).
//
// Two commands are NOT converted here — their logic stays entirely in this
// file because it depends on genuinely Tauri-only state with no
// `ServiceContext`-based equivalent (see each command's doc comment):
// `get_frame_preview` (partially — only the SQL lookup moved) and
// `get_database_path` (fully un-converted).

use crate::models::*;
use tauri::{AppHandle, Manager, State};

use athenaeum_core::api::files as api;
use athenaeum_core::api::PathPolicy;

use super::AppState;

pub use athenaeum_core::api::files::{CatalogSearchHit, DirectoryContents};

/// Get files from the database with optional limit
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_files(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<FileWithFrame>, String> {
    api::get_files(&state.ctx, limit).map_err(|e| e.to_string())
}

/// Get files from a specific directory
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_files_by_directory(
    directory_path: String,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<FileWithFrame>, String> {
    api::get_files_by_directory(&state.ctx, directory_path, limit).map_err(|e| e.to_string())
}

/// Get frames with missing metadata
/// category: "all", "coordinates", "object", "datetime", "instrument", "frametype"
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_frames_with_missing_metadata(
    category: String,
    state: State<'_, AppState>,
) -> Result<Vec<MissingMetadataRow>, String> {
    api::get_frames_with_missing_metadata(&state.ctx, category).map_err(|e| e.to_string())
}

/// Bulk-update metadata fields (camera / date_obs / imagetyp / is_master) on
/// a set of frames. DB-only — the FITS files on disk are NOT rewritten.
/// Returns the number of rows updated.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn bulk_update_frame_metadata(
    frame_ids: Vec<i64>,
    edits: FrameMetadataEdits,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    api::bulk_update_frame_metadata(&state.ctx, frame_ids, edits).map_err(|e| e.to_string())
}

/// Re-decode the originally-scanned header values for the given frames
/// out of `fits_header` so the UI can show "what the file said before
/// you edited it" and offer per-field revert.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_frame_metadata_originals(
    frame_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<Vec<athenaeum_core::fits_parser::stored_header::FrameOriginalSnapshot>, String> {
    api::get_frame_metadata_originals(&state.ctx, frame_ids).map_err(|e| e.to_string())
}

/// Aggregate a summary of which framesets / calibration sets the given
/// frames belong to (member of) or use (consumer of). Drives the metadata
/// pane's "Memberships" panel.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_frame_memberships(
    frame_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<athenaeum_core::db::FrameMembershipsSummary, String> {
    api::get_frame_memberships(&state.ctx, frame_ids).map_err(|e| e.to_string())
}

/// Count the calibration-set / session relations the given frames currently
/// have. The dual-pane metadata editor calls this before applying edits so
/// the user can see how many links the cascade will sever.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn count_frame_metadata_relations(
    frame_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<athenaeum_core::db::FrameMetadataRelations, String> {
    api::count_frame_metadata_relations(&state.ctx, frame_ids).map_err(|e| e.to_string())
}

/// Return the distinct non-empty INSTRUME values from the `frames` table,
/// alphabetically sorted. Feeds the Set Camera modal's existing-cameras list.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_distinct_instrumes(
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    api::get_distinct_instrumes(&state.ctx).map_err(|e| e.to_string())
}

/// Get duplicate file groups (from cache)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_duplicates(state: State<'_, AppState>) -> Result<Vec<DuplicateGroup>, String> {
    api::get_duplicates(&state.ctx).map_err(|e| e.to_string())
}

/// Get directory contents (subdirectories and files)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_directory_contents(
    directory_path: String,
    state: State<'_, AppState>,
) -> Result<DirectoryContents, String> {
    api::get_directory_contents(&state.ctx, directory_path).map_err(|e| e.to_string())
}

/// Get frame preview image as base64-encoded JPEG
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
pub async fn get_frame_preview(
    state: State<'_, AppState>,
    frame_id: i64,
    resolution: Option<String>,
) -> Result<String, String> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    // SQL lookup is single-sourced (`api::get_frame_preview_path`); the
    // render + base64-encode below is Tauri-only glue — see that handler's
    // doc comment for why it can't move to `athenaeum-core`.
    let file_path = api::get_frame_preview_path(&state.ctx, frame_id).map_err(|e| e.to_string())?;

    // Use the existing rustafits command to get preview
    let jpeg_data = crate::commands_rustafits::read_fits_image_rustafits(
        file_path,
        resolution.or(Some("thumbnail".to_string())),
        state,
    )
    .await
    .map_err(|e| format!("Failed to generate preview: {}", e))?;

    // Encode as base64 for SVG embedding
    let base64_data = STANDARD.encode(&jpeg_data);
    Ok(format!("data:image/jpeg;base64,{}", base64_data))
}

/// Get files with frames by frame IDs
/// Useful for loading full file data when you only have frame IDs
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_files_with_frames_by_ids(
    frame_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<Vec<FileWithFrame>, String> {
    api::get_files_with_frames_by_ids(&state.ctx, frame_ids).map_err(|e| e.to_string())
}

/// Get the path to the log directory (rolling daily JSONL files live inside it).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_log_path() -> Result<String, String> {
    api::get_log_path().map_err(|e| e.to_string())
}

/// Get the path to the database file
///
/// NOT single-sourced: needs `tauri::AppHandle::path().app_data_dir()`, which
/// has no `ServiceContext` equivalent and is fundamentally different in kind
/// from the web route's version (which reads the *actual* configured path
/// off the live `Database` handle via `ctx.db.get()`). See
/// `athenaeum_core::api::files` module doc for why this stays here.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_database_path(
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let app_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    Ok(app_dir.join("athenaeum.db").to_string_lossy().to_string())
}

/// Get all distinct directory paths containing files for a given camera
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_camera_directories(
    instrume: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    api::get_camera_directories(&state.ctx, instrume).map_err(|e| e.to_string())
}

/// Get directory contents filtered to only files from a specific camera
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_camera_directory_contents(
    directory_path: String,
    instrume: String,
    camera_directories: Vec<String>,
    state: State<'_, AppState>,
) -> Result<DirectoryContents, String> {
    api::get_camera_directory_contents(&state.ctx, directory_path, instrume, camera_directories)
        .map_err(|e| e.to_string())
}

// ============================================================================
// Dual-pane file browser commands (Phase 1: Move + catalog search)
// ============================================================================

/// Plan + immediately enqueue a Move operation. Returns the operation_id
/// once the plan is committed. The actual move runs on the global queue.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn enqueue_move_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    sources: Vec<String>,
    dest_dir: String,
) -> Result<i64, String> {
    let emitter = crate::tauri_events::TauriProgressEmitter(app);
    api::enqueue_move_operation(&state.ctx, sources, dest_dir, &PathPolicy::AllowAll, emitter)
        .map_err(|e| e.to_string())
}

/// Free-text catalog search across all scan roots. Returns up to `limit`
/// hits matching by filename, OBJECT, FILTER, IMAGETYP, INSTRUME, or
/// TELESCOP (case-insensitive substring match).
///
/// `instrume_filter` constrains hits to a single camera (frames.instrume
/// equality). Used by the per-camera dual-pane file browser so a camera-
/// scoped view's search bar can't surface frames from other cameras.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn search_catalog(
    state: State<'_, AppState>,
    query: String,
    limit: Option<u32>,
    instrume_filter: Option<String>,
) -> Result<Vec<CatalogSearchHit>, String> {
    api::search_catalog(&state.ctx, query, limit, instrume_filter).map_err(|e| e.to_string())
}

/// Map a set of selected paths (files and/or folders) to catalog frame ids,
/// so the dual-pane file browser (which selects by path) can feed the Send
/// dialog (which addresses frames by id). Folders resolve recursively;
/// overlapping file + folder selections are deduped; non-cataloged paths
/// contribute nothing.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn resolve_frame_ids_for_paths(
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> Result<Vec<i64>, String> {
    api::resolve_frame_ids_for_paths(&state.ctx, paths).map_err(|e| e.to_string())
}

/// Create a directory inside a scan root. Validated to refuse paths that
/// would land outside any configured scan root.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn mkdir_in_scan_root(
    state: State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    api::mkdir_in_scan_root(&state.ctx, path, &PathPolicy::AllowAll).map_err(|e| e.to_string())
}

/// Rename a file or directory in place. Same-folder rename only (i.e., no
/// path traversal). Hot-syncs the catalog: for a file, updates `files.path`
/// and `files.filename`; for a directory, updates every `files.path` whose
/// path begins with the old directory path.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn rename_path(
    state: State<'_, AppState>,
    old_path: String,
    new_name: String,
) -> Result<(), String> {
    api::rename_path(&state.ctx, old_path, new_name, &PathPolicy::AllowAll).map_err(|e| e.to_string())
}
