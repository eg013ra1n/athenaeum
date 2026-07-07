// Personal-sync route handlers (Stage I, task A7) — thin wrappers only.
// Business logic lives in `athenaeum_core::api::sync`; mirrors the Tauri
// commands in `crates/athenaeum-tauri/src/commands/sync.rs`.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Json};

use athenaeum_core::api::sync as api;
use athenaeum_core::api::sync::{EnqueueSelectionResult, SyncHistoryQuery};
use athenaeum_core::sync::{HistoryRow, SyncStatus};
use serde::Deserialize;

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::WebAppState;

/// POST /api/get_sync_pairing_ticket
///
/// Dev-flagged: lazily starts the receiver + iroh transport and returns this
/// device's pairing ticket. The spawned receiver emits `sync-progress` /
/// `sync-finished` over SSE.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_sync_pairing_ticket(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<String>, (StatusCode, String)> {
    let emitter = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    api::get_pairing_ticket(&state.ctx, &state.sync, emitter)
        .await
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_sync_status
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_sync_status(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<SyncStatus>, (StatusCode, String)> {
    api::get_status(&state.ctx, &state.sync).await.map(Json).map_err(api_err)
}

/// POST /api/list_sync_history
#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_sync_history(
    State(state): State<WebAppState>,
    Json(query): Json<SyncHistoryQuery>,
) -> Result<Json<Vec<HistoryRow>>, (StatusCode, String)> {
    api::list_history(&state.ctx, query).map(Json).map_err(api_err)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnqueueSelectionArgs {
    pub frame_ids: Vec<i64>,
}

/// POST /api/enqueue_sync_selection
#[tracing::instrument(skip_all, err(Debug))]
pub async fn enqueue_sync_selection(
    State(state): State<WebAppState>,
    Json(args): Json<EnqueueSelectionArgs>,
) -> Result<Json<EnqueueSelectionResult>, (StatusCode, String)> {
    api::enqueue_sync_selection(&state.ctx, &state.sync_sender, args.frame_ids)
        .await
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_sync_auto_mode
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_sync_auto_mode(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<bool>, (StatusCode, String)> {
    api::get_sync_auto_mode(&state.ctx).map(Json).map_err(api_err)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAutoModeArgs {
    pub enabled: bool,
}

/// POST /api/set_sync_auto_mode
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_sync_auto_mode(
    State(state): State<WebAppState>,
    Json(args): Json<SetAutoModeArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::set_sync_auto_mode(&state.ctx, args.enabled).map(Json).map_err(api_err)
}
