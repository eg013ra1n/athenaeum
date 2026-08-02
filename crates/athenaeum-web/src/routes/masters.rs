// Master-build route handlers — business logic single-sourced in
// `athenaeum_core::api::masters`. Covers Task 12 (preview/start/cancel/
// provenance) plus Task 13 (dependency-ordered batch builds + in-place
// rebuild).
//
// Thin wrappers only: extraction + handler call + error mapping.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use athenaeum_core::api::masters as api;

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::WebAppState;

pub use athenaeum_core::api::masters::{
    BatchBuildReport, DeleteMasterResult, MasterBuildPreview, MasterProvenanceInfo, MasterRecipe,
};

// ── Request structs ───────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewMasterBuildArgs {
    pub set_id: i64,
    pub recipe: MasterRecipe,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartMasterBuildArgs {
    pub set_id: i64,
    pub recipe: MasterRecipe,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartMasterBuildsBatchArgs {
    pub set_ids: Vec<i64>,
    pub recipe: MasterRecipe,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebuildMasterArgs {
    pub master_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelMasterBuildArgs {
    pub set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteMasterArgs {
    pub master_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetMasterProvenanceArgs {
    pub master_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveCalibrationOriginalsArgs {
    pub calibration_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreCalibrationOriginalsArgs {
    pub calibration_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearStaleArchiveMarkersArgs {
    pub calibration_set_id: i64,
}

// ── Handlers ─────────────────────────────────────────────────────────────

/// POST /api/preview_master_build
///
/// Pure DB work (no pixel I/O — precal pixels only ever load inside the
/// build thread via `load_precal_pixels`), but still run under
/// `spawn_blocking` so the queries stay off the async executor (matches the
/// `analyze_frame_set` wrapper precedent).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn preview_master_build(
    State(state): State<WebAppState>,
    Json(args): Json<PreviewMasterBuildArgs>,
) -> Result<Json<MasterBuildPreview>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        api::preview_master_build(&ctx, args.set_id, &args.recipe)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Preview task panicked: {}", e)))?
    .map_err(api_err)?;

    Ok(Json(result))
}

/// POST /api/start_master_build
///
/// Validates, registers the cancel handle, and spawns a dedicated named
/// thread that does the actual queue-admission + integration + write +
/// register; returns as soon as the thread is spawned.
/// `master-build-progress` / `master-build-complete` SSE events are emitted
/// via `SseProgressEmitter` from that thread.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn start_master_build(
    State(state): State<WebAppState>,
    Json(args): Json<StartMasterBuildArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let emitter = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    api::start_master_build(
        state.ctx.clone(),
        emitter,
        env!("CARGO_PKG_VERSION").to_string(),
        args.set_id,
        args.recipe,
    )
    .map(Json)
    .map_err(api_err)
}

/// POST /api/start_master_builds_batch
///
/// Dependency-ordered fan-out of `start_master_build` over many sets (see
/// `api::masters::plan_batch`). Sets already superseded / too small /
/// themselves masters / unknown are skipped with a per-set reason instead of
/// failing the whole batch.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn start_master_builds_batch(
    State(state): State<WebAppState>,
    Json(args): Json<StartMasterBuildsBatchArgs>,
) -> Result<Json<BatchBuildReport>, (StatusCode, String)> {
    let emitter = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    api::start_master_builds_batch(
        state.ctx.clone(),
        emitter,
        env!("CARGO_PKG_VERSION").to_string(),
        args.set_ids,
        args.recipe,
    )
    .map(Json)
    .map_err(api_err)
}

/// POST /api/rebuild_master
///
/// Re-integrates an existing built master in place (same path), refreshing
/// its provenance instead of registering a new master set.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn rebuild_master(
    State(state): State<WebAppState>,
    Json(args): Json<RebuildMasterArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let emitter = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    api::rebuild_master(
        state.ctx.clone(),
        emitter,
        env!("CARGO_PKG_VERSION").to_string(),
        args.master_set_id,
    )
    .map(Json)
    .map_err(api_err)
}

/// POST /api/cancel_master_build
#[tracing::instrument(skip_all, err(Debug))]
pub async fn cancel_master_build(
    State(state): State<WebAppState>,
    Json(args): Json<CancelMasterBuildArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::cancel_master_build(&state.ctx, args.set_id).map(Json).map_err(api_err)
}

/// POST /api/delete_master
///
/// Un-supersede the master's raw source set, repoint consumers back onto it,
/// drop the master's catalog rows and its file from disk. 409 while a build
/// for the same lineage is in flight.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_master(
    State(state): State<WebAppState>,
    Json(args): Json<DeleteMasterArgs>,
) -> Result<Json<DeleteMasterResult>, (StatusCode, String)> {
    api::delete_master(&state.ctx, args.master_set_id).map(Json).map_err(api_err)
}

/// POST /api/get_master_provenance
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_master_provenance(
    State(state): State<WebAppState>,
    Json(args): Json<GetMasterProvenanceArgs>,
) -> Result<Json<Option<MasterProvenanceInfo>>, (StatusCode, String)> {
    api::get_master_provenance(&state.ctx, args.master_set_id).map(Json).map_err(api_err)
}

/// POST /api/archive_calibration_originals
///
/// Archive a SUPERSEDED calibration set's original member frames into a ZIP
/// (Task 14). Thin wrapper: plan+commit happen synchronously inside
/// `api::masters::archive_originals`, which returns as soon as the operation
/// is queued on the shared disk worker. Progress/completion arrive via the
/// existing `archive-progress` / `archive-finished` SSE events, same as the
/// frame-set archive flow.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn archive_calibration_originals(
    State(state): State<WebAppState>,
    Json(args): Json<ArchiveCalibrationOriginalsArgs>,
) -> Result<Json<i64>, (StatusCode, String)> {
    let emitter = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    api::archive_originals(state.ctx.clone(), emitter, args.calibration_set_id)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/restore_calibration_originals
///
/// Restore a SUPERSEDED calibration set's archived originals from their zip
/// (mirror of `archive_calibration_originals`, reverse direction). Thin
/// wrapper: op-id resolution + zip-exists validation happen synchronously
/// inside `api::masters::restore_originals` (returns an actionable error if
/// the zip is missing), which then enqueues the restore on the shared disk
/// worker exactly like `archive_originals` does. Progress/completion arrive
/// via the existing `archive-progress` / `archive-finished` SSE events
/// (`kind: "restore"`), same as the frame-set restore flow.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn restore_calibration_originals(
    State(state): State<WebAppState>,
    Json(args): Json<RestoreCalibrationOriginalsArgs>,
) -> Result<Json<i64>, (StatusCode, String)> {
    let emitter = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    api::restore_originals(state.ctx.clone(), emitter, args.calibration_set_id)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/clear_stale_archive_markers
///
/// Clear the stale archive markers on a calibration set whose archive zip
/// was deleted outside the app — the "Forget archive" escape hatch from the
/// otherwise-dead missing-zip state. Gated in core on the zip genuinely
/// being missing (409 if it still exists — Restore is the right tool then).
/// Returns the number of member files cleared. Synchronous DB work, no
/// worker/events involved.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn clear_stale_archive_markers(
    State(state): State<WebAppState>,
    Json(args): Json<ClearStaleArchiveMarkersArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    api::clear_stale_archive_markers(&state.ctx, args.calibration_set_id)
        .map(Json)
        .map_err(api_err)
}
