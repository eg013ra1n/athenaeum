// Light-calibration commands (B5 Task 7) — readiness query plus start/cancel
// of the compute-queue-backed light-calibration batch. Mirrors the master-build
// wrappers (`commands/masters.rs`): business logic lives in
// `athenaeum_core::api::lights`; these are thin extraction + call + error-map
// wrappers.

use std::sync::Arc;

use tauri::State;

use athenaeum_core::api::lights as api;

use crate::tauri_events::TauriProgressEmitter;

use super::AppState;

pub use athenaeum_core::api::lights::{LightCalReadiness, LightCalScope};

/// Readiness summary + per-frame status for a frame set's LIGHT members.
/// Pure DB work (no pixel I/O) but run under `spawn_blocking` so the queries
/// stay off the async executor (matches the `preview_master_build` precedent).
/// `flat_norm` mirrors the dialog's "Normalize master flat" toggle — it feeds
/// the derived-status staleness check.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_light_calibration_readiness(
    state: State<'_, AppState>,
    set_id: i64,
    flat_norm: bool,
) -> Result<LightCalReadiness, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::get_light_calibration_readiness(&ctx, set_id, flat_norm))
        .await
        .map_err(|e| format!("Readiness task panicked: {}", e))?
        .map_err(|e| e.to_string())
}

/// Start a light-calibration batch. Preflights raw calibration masters,
/// registers the cancel handle, and spawns a dedicated named thread that does
/// queue-admission + per-frame calibration; returns as soon as the thread is
/// spawned. Progress/completion arrive via `calibration-progress` /
/// `calibration-finished` events emitted through `TauriProgressEmitter`.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn start_light_calibration(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    set_id: i64,
    scope: LightCalScope,
    flat_norm: bool,
) -> Result<(), String> {
    let emitter = Arc::new(TauriProgressEmitter(app_handle));
    api::start_light_calibration(
        state.ctx.clone(),
        emitter,
        env!("CARGO_PKG_VERSION").to_string(),
        set_id,
        scope,
        flat_norm,
    )
    .map_err(|e| e.to_string())
}

/// Cancel an active light-calibration batch (queued in the compute queue or
/// running). Sets the cancel flag and drops any known queue ticket.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_light_calibration(
    state: State<'_, AppState>,
    set_id: i64,
) -> Result<(), String> {
    api::cancel_light_calibration(&state.ctx, set_id).map_err(|e| e.to_string())
}
