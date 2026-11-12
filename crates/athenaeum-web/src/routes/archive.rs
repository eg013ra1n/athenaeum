// Archive feature route handlers for the Axum web API.
//
// Mirrors the Tauri archive commands. Progress events are broadcast via SSE
// instead of Tauri's emit mechanism.

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::WebAppState;
use athenaeum_core::api::archive as archive_api;
use athenaeum_core::archive::models::*;
use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;
use std::sync::Arc;

// ── Request structs ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationIdRequest {
    pub operation_id: i64,
}

#[derive(Deserialize)]
pub struct SetCompressionRequest {
    pub compression: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRequest {
    pub frames_set_id: i64,
    pub dispositions: Dispositions,
    pub compression: ArchiveCompression,
    #[serde(default)]
    pub archive_root_path: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRequest {
    pub frames_set_id: i64,
    pub dispositions: Dispositions,
    pub compression: ArchiveCompression,
    pub conflict_resolution: ConflictResolution,
    #[serde(default)]
    pub archive_root_path: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddArchiveRootRequest {
    pub path: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveRootIdRequest {
    pub id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRestoreRequest {
    pub operation_id: i64,
    pub target_root_path: String,
    pub overwrite_existing: bool,
    pub keep_zip_after_restore: bool,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_archive_settings(
    State(state): State<WebAppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    archive_api::get_archive_settings(&state.ctx)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_archive_compression(
    State(state): State<WebAppState>,
    Json(req): Json<SetCompressionRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    archive_api::set_archive_compression(&state.ctx, &req.compression).map_err(api_err)?;
    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn plan_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<PlanRequest>,
) -> Result<Json<ArchivePlan>, (StatusCode, String)> {
    archive_api::plan_archive_operation(
        &state.ctx,
        req.frames_set_id,
        &req.dispositions,
        req.compression,
        req.archive_root_path.as_deref(),
    )
    .map(Json)
    .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn start_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<StartRequest>,
) -> Result<Json<i64>, (StatusCode, String)> {
    // The web refuses a second archive while one is active (409); the desktop
    // queues it. The difference is the `refuse_when_busy` argument.
    archive_api::start_archive_operation(
        state.ctx.clone(),
        Arc::new(SseProgressEmitter::new(state.event_tx.clone())),
        req.frames_set_id,
        &req.dispositions,
        req.compression,
        req.conflict_resolution,
        req.archive_root_path.as_deref(),
        true,
    )
    .map(Json)
    .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn cancel_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    archive_api::cancel_archive_operation(&state.ctx, req.operation_id).map_err(api_err)?;
    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_restore_suggestions(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    archive_api::get_restore_suggestions(&state.ctx, req.operation_id)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_unfinished_archive_operations(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<ArchiveOperationSummary>>, (StatusCode, String)> {
    archive_api::list_unfinished_archive_operations(&state.ctx)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn resume_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    archive_api::resume_archive_operation(
        state.ctx.clone(),
        Arc::new(SseProgressEmitter::new(state.event_tx.clone())),
        req.operation_id,
    )
    .map_err(api_err)?;
    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn rollback_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    archive_api::rollback_archive_operation(
        state.ctx.clone(),
        Arc::new(SseProgressEmitter::new(state.event_tx.clone())),
        req.operation_id,
    )
    .map_err(api_err)?;
    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_archive_zips(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    archive_api::list_archive_zips(&state.ctx, req.operation_id)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_archived_frame_sets(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    archive_api::list_archived_frame_sets(&state.ctx)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn start_restore_operation(
    State(state): State<WebAppState>,
    Json(req): Json<StartRestoreRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let emitter = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    archive_api::start_restore_operation(
        state.ctx.clone(),
        emitter,
        req.operation_id,
        req.target_root_path,
        req.overwrite_existing,
        req.keep_zip_after_restore,
    )
    .map_err(api_err)?;
    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_archive_roots(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    archive_api::list_archive_roots(&state.ctx)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn add_archive_root(
    State(state): State<WebAppState>,
    Json(req): Json<AddArchiveRootRequest>,
) -> Result<Json<i64>, (StatusCode, String)> {
    archive_api::add_archive_root(&state.ctx, req.path, req.label)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_archive_root(
    State(state): State<WebAppState>,
    Json(req): Json<ArchiveRootIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    archive_api::delete_archive_root(&state.ctx, req.id).map_err(api_err)?;
    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_default_archive_root(
    State(state): State<WebAppState>,
    Json(req): Json<ArchiveRootIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    archive_api::set_default_archive_root(&state.ctx, req.id).map_err(api_err)?;
    Ok(StatusCode::OK)
}
