// File route handlers — mirrors athenaeum-tauri/src/commands/files.rs
//
// Thin wrappers only: extraction + policy construction + handler call + error
// mapping. Business logic lives in athenaeum_core::api::files (Task 10
// conversion — see .superpowers/sdd/p1-task-10-report.md).
//
// NOTE: two Tauri commands whose bodies live in commands/files.rs have their
// web counterpart in OTHER route modules, not here: `get_duplicates` is in
// `routes/duplicates.rs`, and `get_frame_preview` is in `routes/images.rs`.
// Both are out of this file's declared scope; their `api::files` handlers
// exist (used by the Tauri side) but these two web routes still carry their
// own (pre-conversion, unmodified) inline logic. See the Task 10 report.

use athenaeum_core::api::files as api;
use athenaeum_core::api::PathPolicy;
use athenaeum_core::models::{FileWithFrame, FrameMetadataEdits, MissingMetadataRow};
use axum::{extract::State, http::StatusCode, Json};
use std::path::PathBuf;

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::WebAppState;

pub use athenaeum_core::api::files::{BrowseDirectoriesResponse, CatalogSearchHit, DirectoryContents};

// ── Request structs ──────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct GetFilesArgs {
    pub limit: Option<usize>,
}

#[derive(serde::Deserialize)]
pub struct GetFilesByDirectoryArgs {
    #[serde(rename = "directoryPath")]
    pub directory_path: String,
    pub limit: Option<usize>,
}

#[derive(serde::Deserialize)]
pub struct GetDirectoryContentsArgs {
    #[serde(rename = "directoryPath")]
    pub path: String,
}

#[derive(serde::Deserialize)]
pub struct GetCameraDirectoriesArgs {
    pub instrume: String,
}

#[derive(serde::Deserialize)]
pub struct GetCameraDirectoryContentsArgs {
    pub instrume: String,
    #[serde(rename = "directoryPath")]
    pub directory_path: String,
    #[serde(rename = "cameraDirectories")]
    pub camera_directories: Vec<String>,
}

#[derive(serde::Deserialize)]
pub struct GetFramesWithMissingMetadataArgs {
    /// Category filter: "all", "coordinates", "object", "datetime", "instrument", "frametype"
    pub category: String,
}

#[derive(serde::Deserialize)]
pub struct GetFilesWithFramesByIdsArgs {
    #[serde(rename = "frameIds")]
    pub frame_ids: Vec<i64>,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// POST /api/get_files
///
/// Returns files with their frame metadata, most recently created first.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_files(
    State(state): State<WebAppState>,
    Json(args): Json<GetFilesArgs>,
) -> Result<Json<Vec<FileWithFrame>>, (StatusCode, String)> {
    api::get_files(&state.ctx, args.limit).map(Json).map_err(api_err)
}

/// POST /api/get_files_by_directory
///
/// Returns files directly inside the given directory (non-recursive), with
/// their frame metadata. The `directoryPath` must be an absolute path.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_files_by_directory(
    State(state): State<WebAppState>,
    Json(args): Json<GetFilesByDirectoryArgs>,
) -> Result<Json<Vec<FileWithFrame>>, (StatusCode, String)> {
    api::get_files_by_directory(&state.ctx, args.directory_path, args.limit)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_directory_contents
///
/// Returns the immediate subdirectories of `path` (via the filesystem) and the
/// files directly inside it (from the database).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_directory_contents(
    State(state): State<WebAppState>,
    Json(args): Json<GetDirectoryContentsArgs>,
) -> Result<Json<DirectoryContents>, (StatusCode, String)> {
    api::get_directory_contents(&state.ctx, args.path).map(Json).map_err(api_err)
}

/// POST /api/get_camera_directories
///
/// Returns all distinct directory paths that contain files from the given
/// camera (`instrume`), ordered alphabetically.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_camera_directories(
    State(state): State<WebAppState>,
    Json(args): Json<GetCameraDirectoriesArgs>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    api::get_camera_directories(&state.ctx, args.instrume).map(Json).map_err(api_err)
}

/// POST /api/get_camera_directory_contents
///
/// Returns the immediate subdirectories of `directoryPath` that are ancestors
/// of (or equal to) a camera directory, plus the files directly inside it
/// that belong to the given camera.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_camera_directory_contents(
    State(state): State<WebAppState>,
    Json(args): Json<GetCameraDirectoryContentsArgs>,
) -> Result<Json<DirectoryContents>, (StatusCode, String)> {
    api::get_camera_directory_contents(&state.ctx, args.directory_path, args.instrume, args.camera_directories)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_frames_with_missing_metadata
///
/// Returns light frames (and optionally frames with no imagetyp) that have
/// incomplete metadata. The `category` field controls which metadata gap to
/// filter on: "all", "coordinates", "object", "datetime", "instrument", or
/// "frametype".
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frames_with_missing_metadata(
    State(state): State<WebAppState>,
    Json(args): Json<GetFramesWithMissingMetadataArgs>,
) -> Result<Json<Vec<MissingMetadataRow>>, (StatusCode, String)> {
    api::get_frames_with_missing_metadata(&state.ctx, args.category)
        .map(Json)
        .map_err(api_err)
}

// ── Bulk frame metadata edits ────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct BulkUpdateFrameMetadataArgs {
    #[serde(rename = "frameIds")]
    pub frame_ids: Vec<i64>,
    pub edits: FrameMetadataEdits,
}

#[derive(serde::Deserialize)]
pub struct CountFrameMetadataRelationsArgs {
    #[serde(rename = "frameIds")]
    pub frame_ids: Vec<i64>,
}

/// POST /api/get_frame_metadata_originals
///
/// Re-decode the originally-scanned header values for given frames so the
/// UI can compare against current edits and revert per field.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frame_metadata_originals(
    State(state): State<WebAppState>,
    Json(args): Json<CountFrameMetadataRelationsArgs>,
) -> Result<Json<Vec<athenaeum_core::fits_parser::stored_header::FrameOriginalSnapshot>>, (StatusCode, String)> {
    api::get_frame_metadata_originals(&state.ctx, args.frame_ids)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_frame_memberships
///
/// Aggregate which framesets / calibration sets the given frames belong to.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frame_memberships(
    State(state): State<WebAppState>,
    Json(args): Json<CountFrameMetadataRelationsArgs>,
) -> Result<Json<athenaeum_core::db::FrameMembershipsSummary>, (StatusCode, String)> {
    api::get_frame_memberships(&state.ctx, args.frame_ids).map(Json).map_err(api_err)
}

/// POST /api/count_frame_metadata_relations
///
/// Counts the calibration-set / session relations for the given frames so
/// the metadata editor can warn the user before applying edits that will
/// unlink them.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn count_frame_metadata_relations(
    State(state): State<WebAppState>,
    Json(args): Json<CountFrameMetadataRelationsArgs>,
) -> Result<Json<athenaeum_core::db::FrameMetadataRelations>, (StatusCode, String)> {
    api::count_frame_metadata_relations(&state.ctx, args.frame_ids)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/bulk_update_frame_metadata
///
/// DB-only bulk update of camera / date_obs / imagetyp / is_master on the given
/// frames. Used by the Missing Metadata page's Set Camera / Set Date / Set
/// Frame Type actions. Returns the number of rows updated.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn bulk_update_frame_metadata(
    State(state): State<WebAppState>,
    Json(args): Json<BulkUpdateFrameMetadataArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    api::bulk_update_frame_metadata(&state.ctx, args.frame_ids, args.edits)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_distinct_instrumes
///
/// Returns the distinct non-empty INSTRUME values from the frames table,
/// alphabetically sorted. Feeds the Set Camera modal dropdown.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_distinct_instrumes(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    api::get_distinct_instrumes(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/get_files_with_frames_by_ids
///
/// Bulk-loads full file and frame records for the given list of frame IDs.
/// Useful when you have frame IDs from a frame set and need the complete
/// metadata for display.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_files_with_frames_by_ids(
    State(state): State<WebAppState>,
    Json(args): Json<GetFilesWithFramesByIdsArgs>,
) -> Result<Json<Vec<FileWithFrame>>, (StatusCode, String)> {
    api::get_files_with_frames_by_ids(&state.ctx, args.frame_ids)
        .map(Json)
        .map_err(api_err)
}

// ── Browse directories (web-only) ──────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct BrowseDirectoriesArgs {
    pub path: Option<String>,
    /// `"scan"` (default) validates against `allowed_paths`;
    /// `"export"` validates against the configured export directory.
    pub scope: Option<String>,
}

/// POST /api/browse_directories
///
/// Returns subdirectories of the given path. If path is empty or omitted,
/// returns the root entries for the requested scope.
///
/// `scope = "scan"` (default): validates against `state.allowed_paths`.
/// `scope = "export"`: validates against the configured export directory.
///
/// Scope-to-root-paths resolution stays here (not in `api::files`) because
/// it depends on `WebAppState::allowed_paths` / `WebAppState::export_dir`,
/// web-only fields with no `ServiceContext` equivalent.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn browse_directories(
    State(state): State<WebAppState>,
    Json(args): Json<BrowseDirectoriesArgs>,
) -> Result<Json<BrowseDirectoriesResponse>, (StatusCode, String)> {
    let scope = args.scope.as_deref().unwrap_or("scan");

    let root_paths: Vec<PathBuf> = match scope {
        "export" => match state.export_dir {
            Some(ref dir) => vec![dir.clone()],
            None => return Err((StatusCode::BAD_REQUEST, "No export directory configured".to_string())),
        },
        _ => state.allowed_paths.clone(),
    };

    api::browse_directories(args.path, &root_paths).map(Json).map_err(api_err)
}

// ============================================================================
// Dual-pane file browser routes (Phase 1: Move + catalog search)
// ============================================================================

#[derive(serde::Deserialize)]
pub struct EnqueueMoveArgs {
    pub sources: Vec<String>,
    #[serde(rename = "destDir")]
    pub dest_dir: String,
}

#[derive(serde::Deserialize)]
pub struct SearchCatalogArgs {
    pub query: String,
    pub limit: Option<u32>,
    /// When set, restricts hits to a single camera (frames.instrume equality).
    /// Used by the per-camera dual-pane file browser so a camera-scoped view
    /// can't surface frames from other cameras. Mirrors the same parameter on
    /// the Tauri side.
    #[serde(default)]
    pub instrume_filter: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct ResolveFrameIdsForPathsArgs {
    pub paths: Vec<String>,
}

#[derive(serde::Deserialize)]
pub struct MkdirArgs {
    pub path: String,
}

#[derive(serde::Deserialize)]
pub struct RenamePathArgs {
    #[serde(rename = "oldPath")]
    pub old_path: String,
    #[serde(rename = "newName")]
    pub new_name: String,
}

/// Builds the path-sandboxing policy from `allowed_paths`, mirroring
/// `routes/scan_roots.rs`'s `allowed_roots_policy` helper (kept as a small
/// per-module copy rather than a shared export — see that module's version
/// for the canonicalization rationale). Canonicalizes each allowed root here;
/// the candidate path is canonicalized inside the shared `api::files`
/// handler — see `PathPolicy::check`'s doc comment for why both sides must
/// be canonical before the lexical `starts_with` check. Empty `allowed_paths`
/// yields `AllowAll`, matching the pre-conversion `if !allowed.is_empty()`
/// short-circuit used by every path-validated command in this file.
fn allowed_roots_policy(allowed_paths: &[PathBuf]) -> PathPolicy {
    if allowed_paths.is_empty() {
        PathPolicy::AllowAll
    } else {
        PathPolicy::AllowedRoots(
            allowed_paths.iter().map(|p| p.canonicalize().unwrap_or_else(|_| p.clone())).collect(),
        )
    }
}

/// POST /api/enqueue_move_operation
#[tracing::instrument(skip_all, err(Debug))]
pub async fn enqueue_move_operation(
    State(state): State<WebAppState>,
    Json(args): Json<EnqueueMoveArgs>,
) -> Result<Json<i64>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    let emitter = SseProgressEmitter::new(state.event_tx.clone());
    let op_id = api::enqueue_move_operation(&state.ctx, args.sources, args.dest_dir, &policy, emitter)
        .map_err(api_err)?;
    Ok(Json(op_id))
}

/// POST /api/search_catalog
#[tracing::instrument(skip_all, err(Debug))]
pub async fn search_catalog(
    State(state): State<WebAppState>,
    Json(args): Json<SearchCatalogArgs>,
) -> Result<Json<Vec<CatalogSearchHit>>, (StatusCode, String)> {
    api::search_catalog(&state.ctx, args.query, args.limit, args.instrume_filter)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/resolve_frame_ids_for_paths
#[tracing::instrument(skip_all, err(Debug))]
pub async fn resolve_frame_ids_for_paths(
    State(state): State<WebAppState>,
    Json(args): Json<ResolveFrameIdsForPathsArgs>,
) -> Result<Json<Vec<i64>>, (StatusCode, String)> {
    api::resolve_frame_ids_for_paths(&state.ctx, args.paths)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/mkdir_in_scan_root
#[tracing::instrument(skip_all, err(Debug))]
pub async fn mkdir_in_scan_root(
    State(state): State<WebAppState>,
    Json(args): Json<MkdirArgs>,
) -> Result<StatusCode, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    api::mkdir_in_scan_root(&state.ctx, args.path, &policy).map_err(api_err)?;
    Ok(StatusCode::OK)
}

/// POST /api/rename_path
#[tracing::instrument(skip_all, err(Debug))]
pub async fn rename_path(
    State(state): State<WebAppState>,
    Json(args): Json<RenamePathArgs>,
) -> Result<StatusCode, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    api::rename_path(&state.ctx, args.old_path, args.new_name, &policy).map_err(api_err)?;
    Ok(StatusCode::OK)
}
