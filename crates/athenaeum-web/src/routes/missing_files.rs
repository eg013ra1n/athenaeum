// Missing files route handlers — mirrors athenaeum-tauri/src/commands/missing_files.rs

use axum::{extract::State, http::StatusCode, Json};

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::WebAppState;

// ── Response DTOs ─────────────────────────────────────────────────────────────

pub use athenaeum_core::api::missing_files::MissingFileRecord;

// ── Request body types ────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootIdArgs {
    pub root_id: i64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncMissingFilesArgs {
    pub root_id: i64,
    pub file_ids: Vec<i64>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIdArgs {
    pub file_id: i64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteMissingFilesArgs {
    pub file_ids: Vec<i64>,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// POST /api/check_missing_files_in_scan_root
///
/// Check for files in the DB under this root that no longer exist on disk.
/// Emits `verifying` SSE progress before and after the filesystem check
/// (the shared core handler does both).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn check_missing_files_in_scan_root(
    State(state): State<WebAppState>,
    Json(args): Json<RootIdArgs>,
) -> Result<Json<Vec<athenaeum_core::models::OrphanedFile>>, (StatusCode, String)> {
    let emitter = SseProgressEmitter::new(state.event_tx.clone());
    athenaeum_core::api::scan_roots::check_missing_files_in_scan_root(
        &state.ctx,
        args.root_id,
        &emitter,
    )
    .map(Json)
    .map_err(api_err)
}

/// POST /api/sync_missing_files
///
/// Sync the missing_files table after a rescan. Inserts new missing entries,
/// removes entries for files that now exist, preserves 'ignored' entries.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn sync_missing_files(
    State(state): State<WebAppState>,
    Json(args): Json<SyncMissingFilesArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    athenaeum_core::api::missing_files::sync_missing_files(&state.ctx, args.root_id, &args.file_ids)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_missing_files
///
/// Return all missing file records for a scan root with full details.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_missing_files(
    State(state): State<WebAppState>,
    Json(args): Json<RootIdArgs>,
) -> Result<Json<Vec<MissingFileRecord>>, (StatusCode, String)> {
    athenaeum_core::api::missing_files::get_missing_files(&state.ctx, args.root_id)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/recheck_missing_files
///
/// Re-verify all missing files for a root on disk, removing entries for files
/// that have reappeared, then return the updated list.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn recheck_missing_files(
    State(state): State<WebAppState>,
    Json(args): Json<RootIdArgs>,
) -> Result<Json<Vec<MissingFileRecord>>, (StatusCode, String)> {
    athenaeum_core::api::missing_files::recheck_missing_files(&state.ctx, args.root_id)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/ignore_missing_file
#[tracing::instrument(skip_all, err(Debug))]
pub async fn ignore_missing_file(
    State(state): State<WebAppState>,
    Json(args): Json<FileIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    athenaeum_core::api::missing_files::ignore_missing_file(&state.ctx, args.file_id)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/unignore_missing_file
#[tracing::instrument(skip_all, err(Debug))]
pub async fn unignore_missing_file(
    State(state): State<WebAppState>,
    Json(args): Json<FileIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    athenaeum_core::api::missing_files::unignore_missing_file(&state.ctx, args.file_id)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/delete_missing_files
///
/// Delete files from the database entirely (CASCADE handles frames, missing_files, etc.)
#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_missing_files(
    State(state): State<WebAppState>,
    Json(args): Json<DeleteMissingFilesArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    athenaeum_core::api::missing_files::delete_missing_files(&state.ctx, &args.file_ids)
        .map(Json)
        .map_err(api_err)
}
