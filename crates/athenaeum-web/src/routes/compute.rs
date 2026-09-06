// Compute-queue inspection/control route handlers — thin wrappers only.
// Business logic lives in `athenaeum_core::api::compute` (mirrors
// `routes/analysis.rs`'s conversion pattern).

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use athenaeum_core::api::compute as api;
use athenaeum_core::services::compute_queue::ComputeQueueEntry;

use crate::routes::api_err;
use crate::WebAppState;

// ── Request structs ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelComputeJobArgs {
    pub job_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetComputeMaxConcurrentArgs {
    pub n: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetIntegrationBandBudgetArgs {
    pub mb: usize,
}

// ── Routes ───────────────────────────────────────────────────────────────────

/// POST /api/get_compute_queue
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_compute_queue(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<Vec<ComputeQueueEntry>>, (StatusCode, String)> {
    Ok(Json(api::get_compute_queue(&state.ctx)))
}

/// POST /api/cancel_compute_job
#[tracing::instrument(skip_all, err(Debug))]
pub async fn cancel_compute_job(
    State(state): State<WebAppState>,
    Json(args): Json<CancelComputeJobArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::cancel_compute_job(&state.ctx, args.job_id).map(Json).map_err(api_err)
}

/// POST /api/set_compute_max_concurrent
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_compute_max_concurrent(
    State(state): State<WebAppState>,
    Json(args): Json<SetComputeMaxConcurrentArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::set_compute_max_concurrent(&state.ctx, args.n).map(Json).map_err(api_err)
}

/// POST /api/get_integration_band_budget
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_integration_band_budget(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<api::IntegrationBudgetInfo>, (StatusCode, String)> {
    api::get_integration_band_budget(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/set_integration_band_budget
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_integration_band_budget(
    State(state): State<WebAppState>,
    Json(args): Json<SetIntegrationBandBudgetArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::set_integration_band_budget(&state.ctx, args.mb).map(Json).map_err(api_err)
}
