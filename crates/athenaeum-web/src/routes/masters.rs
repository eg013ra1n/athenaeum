// Master-build route handlers — business logic single-sourced in
// `athenaeum_core::api::masters`.
//
// Thin wrappers only: extraction + handler call + error mapping.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use athenaeum_core::api::masters as api;

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::WebAppState;

pub use athenaeum_core::api::masters::{MasterBuildPreview, MasterProvenanceInfo, MasterRecipe};

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
pub struct CancelMasterBuildArgs {
    pub set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetMasterProvenanceArgs {
    pub master_set_id: i64,
}

// ── Handlers ─────────────────────────────────────────────────────────────

/// POST /api/preview_master_build
#[tracing::instrument(skip_all, err(Debug))]
pub async fn preview_master_build(
    State(state): State<WebAppState>,
    Json(args): Json<PreviewMasterBuildArgs>,
) -> Result<Json<MasterBuildPreview>, (StatusCode, String)> {
    api::preview_master_build(&state.ctx, args.set_id, &args.recipe).map(Json).map_err(api_err)
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

/// POST /api/cancel_master_build
#[tracing::instrument(skip_all, err(Debug))]
pub async fn cancel_master_build(
    State(state): State<WebAppState>,
    Json(args): Json<CancelMasterBuildArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::cancel_master_build(&state.ctx, args.set_id).map(Json).map_err(api_err)
}

/// POST /api/get_master_provenance
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_master_provenance(
    State(state): State<WebAppState>,
    Json(args): Json<GetMasterProvenanceArgs>,
) -> Result<Json<Option<MasterProvenanceInfo>>, (StatusCode, String)> {
    api::get_master_provenance(&state.ctx, args.master_set_id).map(Json).map_err(api_err)
}
