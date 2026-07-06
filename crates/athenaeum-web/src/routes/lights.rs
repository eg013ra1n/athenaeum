// Light-calibration route handlers (B5 Task 7) — one-for-one mirror of the
// Tauri commands in `athenaeum-tauri/src/commands/lights.rs`. Business logic is
// single-sourced in `athenaeum_core::api::lights`; these are thin extraction +
// call + error-map wrappers, following the master-build route precedent.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use athenaeum_core::api::lights as api;

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::WebAppState;

pub use athenaeum_core::api::lights::{
    FlatNormMode, LightCalParams, LightCalReadiness, LightCalScope,
};

// ── Request structs ───────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetLightCalibrationReadinessArgs {
    pub set_id: i64,
    pub flat_norm: bool,
    pub flat_norm_mode: FlatNormMode,
    /// Advanced parameters — `#[serde(default)]` so an omitted field decodes to
    /// `LightCalParams::default()` (the pre-Advanced-UI frontend omits it).
    #[serde(default)]
    pub params: LightCalParams,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartLightCalibrationArgs {
    pub set_id: i64,
    pub scope: LightCalScope,
    pub flat_norm: bool,
    pub flat_norm_mode: FlatNormMode,
    /// Advanced parameters — `#[serde(default)]` so an omitted field decodes to
    /// `LightCalParams::default()` (the pre-Advanced-UI frontend omits it).
    #[serde(default)]
    pub params: LightCalParams,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelLightCalibrationArgs {
    pub set_id: i64,
}

// ── Handlers ─────────────────────────────────────────────────────────────

/// POST /api/get_light_calibration_readiness
///
/// Pure DB work (no pixel I/O) but run under `spawn_blocking` so the queries
/// stay off the async executor (matches the `preview_master_build` precedent).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_light_calibration_readiness(
    State(state): State<WebAppState>,
    Json(args): Json<GetLightCalibrationReadinessArgs>,
) -> Result<Json<LightCalReadiness>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        api::get_light_calibration_readiness(
            &ctx,
            args.set_id,
            args.flat_norm,
            args.flat_norm_mode,
            args.params,
        )
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Readiness task panicked: {}", e)))?
    .map_err(api_err)?;

    Ok(Json(result))
}

/// POST /api/start_light_calibration
///
/// Preflights raw calibration masters, registers the cancel handle, and spawns
/// a dedicated named thread that does queue-admission + per-frame calibration;
/// returns as soon as the thread is spawned. `calibration-progress` /
/// `calibration-finished` SSE events are emitted via `SseProgressEmitter` from
/// that thread.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn start_light_calibration(
    State(state): State<WebAppState>,
    Json(args): Json<StartLightCalibrationArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let emitter = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    api::start_light_calibration(
        state.ctx.clone(),
        emitter,
        env!("CARGO_PKG_VERSION").to_string(),
        args.set_id,
        args.scope,
        args.flat_norm,
        args.flat_norm_mode,
        args.params,
    )
    .map(Json)
    .map_err(api_err)
}

/// POST /api/cancel_light_calibration
#[tracing::instrument(skip_all, err(Debug))]
pub async fn cancel_light_calibration(
    State(state): State<WebAppState>,
    Json(args): Json<CancelLightCalibrationArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::cancel_light_calibration(&state.ctx, args.set_id).map(Json).map_err(api_err)
}
