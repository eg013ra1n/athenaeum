// Sky Chart HiPS background routes (issue #18). Thin mirrors of
// `commands/hips.rs` in the Tauri shell; logic in `athenaeum_core::api::hips`.

use std::sync::Arc;

use athenaeum_core::api::hips as api;
use athenaeum_core::hips::pack::{PackProgress, PackSelection};
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use super::api_err;
use crate::events::SseProgressEmitter;
use crate::WebAppState;

fn panicked(what: &str, e: tokio::task::JoinError) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{what} task panicked: {e}"))
}

/// `POST /api/get_hips_surveys`
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_hips_surveys(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<api::HipsSurveyInfo>>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::get_hips_surveys(&ctx))
        .await
        .map_err(|e| panicked("HiPS survey", e))?
        .map(Json)
        .map_err(api_err)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HipsTileArgs {
    pub survey_id: String,
    pub order: u8,
    pub npix: u64,
}

/// `POST /api/get_hips_tile` — the tile's bytes with its image content type
/// (the frontend's `httpApi.invoke` turns `image/*` into bytes); an empty
/// body means no coverage there. A POST behind the API key, never an
/// `<img src>` GET. The boundary error is debug: a tile not cached while
/// offline is a normal answer, and a real fetch failure is a `warn` in core.
#[tracing::instrument(skip_all, err(Debug, level = "debug"), level = "debug")]
pub async fn get_hips_tile(
    State(state): State<WebAppState>,
    Json(args): Json<HipsTileArgs>,
) -> Result<Response, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let survey_id = args.survey_id.clone();
    let tile = tokio::task::spawn_blocking(move || api::get_hips_tile(&ctx, &args.survey_id, args.order, args.npix))
        .await
        .map_err(|e| panicked("HiPS tile", e))?
        .map_err(api_err)?;
    Ok(match tile {
        api::HipsTile::Image { bytes, mime } => ([(header::CONTENT_TYPE, mime)], bytes).into_response(),
        api::HipsTile::Absent => {
            let mime = athenaeum_core::hips::survey(&survey_id).map_or("image/jpeg", |s| s.format.mime());
            ([(header::CONTENT_TYPE, mime)], Vec::<u8>::new()).into_response()
        }
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HipsSurveyArgs {
    pub survey_id: String,
}

/// `POST /api/clear_hips_cache`
#[tracing::instrument(skip_all, err(Debug))]
pub async fn clear_hips_cache(
    State(state): State<WebAppState>,
    Json(args): Json<HipsSurveyArgs>,
) -> Result<Json<u64>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::clear_hips_cache(&ctx, &args.survey_id))
        .await
        .map_err(|e| panicked("HiPS cache", e))?
        .map(Json)
        .map_err(api_err)
}

#[derive(Deserialize)]
pub struct HipsPackArgs {
    pub selection: PackSelection,
}

/// `POST /api/estimate_hips_pack`
#[tracing::instrument(skip_all, err(Debug))]
pub async fn estimate_hips_pack(
    State(state): State<WebAppState>,
    Json(args): Json<HipsPackArgs>,
) -> Result<Json<api::HipsPackEstimate>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::estimate_hips_pack(&ctx, &args.selection))
        .await
        .map_err(|e| panicked("HiPS pack estimate", e))?
        .map(Json)
        .map_err(api_err)
}

/// `POST /api/start_hips_pack` — progress goes out as SSE `hips-download-progress`.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn start_hips_pack(
    State(state): State<WebAppState>,
    Json(args): Json<HipsPackArgs>,
) -> Result<Json<PackProgress>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let emitter = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    tokio::task::spawn_blocking(move || api::start_hips_pack(ctx, emitter, args.selection))
        .await
        .map_err(|e| panicked("HiPS pack start", e))?
        .map(Json)
        .map_err(api_err)
}

/// `POST /api/cancel_hips_pack`
#[tracing::instrument(skip_all, err(Debug))]
pub async fn cancel_hips_pack() -> Result<Json<()>, (StatusCode, String)> {
    tokio::task::spawn_blocking(api::cancel_hips_pack)
        .await
        .map_err(|e| panicked("HiPS pack cancel", e))?
        .map(Json)
        .map_err(api_err)
}

/// `POST /api/get_hips_pack_status`
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_hips_pack_status(
    State(state): State<WebAppState>,
    Json(args): Json<HipsSurveyArgs>,
) -> Result<Json<PackProgress>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::get_hips_pack_status(&ctx, &args.survey_id))
        .await
        .map_err(|e| panicked("HiPS pack status", e))?
        .map(Json)
        .map_err(api_err)
}
