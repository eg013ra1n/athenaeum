// Personal-sync route handlers (Stage I, task A7) — thin wrappers only.
// Business logic lives in `athenaeum_core::api::sync`; mirrors the Tauri
// commands in `crates/athenaeum-tauri/src/commands/sync.rs`.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Json};

use athenaeum_core::api::sync as api;
use athenaeum_core::api::sync::{EnqueueSelectionResult, SyncHistoryQuery};
use athenaeum_core::events::ProgressEmitter;
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
    api::get_pairing_ticket(
        Arc::clone(&state.ctx),
        &state.sync,
        Arc::clone(&state.collab_sender),
        emitter,
    )
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
    api::get_status(&state.ctx, &state.sync, &state.sync_sender)
        .await
        .map(Json)
        .map_err(api_err)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListHistoryArgs {
    pub query: SyncHistoryQuery,
}

/// POST /api/list_sync_history
///
/// Body mirrors the Tauri command's single named `query` param
/// (`{ "query": { … } }`), so both backends accept the identical
/// `api.invoke('list_sync_history', { query })` payload.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_sync_history(
    State(state): State<WebAppState>,
    Json(args): Json<ListHistoryArgs>,
) -> Result<Json<Vec<HistoryRow>>, (StatusCode, String)> {
    api::list_history(&state.ctx, args.query).map(Json).map_err(api_err)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnqueueSelectionArgs {
    pub frame_ids: Vec<i64>,
    /// The chosen destination — an account device id resolved to its node id.
    pub destination_device_id: String,
}

/// POST /api/enqueue_sync_selection
#[tracing::instrument(skip_all, err(Debug))]
pub async fn enqueue_sync_selection(
    State(state): State<WebAppState>,
    Json(args): Json<EnqueueSelectionArgs>,
) -> Result<Json<EnqueueSelectionResult>, (StatusCode, String)> {
    // The host emitter, captured into the sender engine on its first spawn, so
    // send-side state transitions surface as `sync-progress`/`sync-finished`.
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    let dest = api::resolve_dest_node(&state.ctx, &args.destination_device_id)
        .await
        .map_err(api_err)?;
    api::enqueue_sync_selection(&state.ctx, &state.sync_sender, dest, args.frame_ids, Some(emitter))
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

/// POST /api/get_sync_device_names
///
/// Map of peer node-id-hex → hub device name for the transfer history. Best-
/// effort: hub unreachable / signed out → empty map (UI falls back to short hex).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_sync_device_names(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<std::collections::HashMap<String, String>>, (StatusCode, String)> {
    api::get_sync_device_names(&state.ctx).await.map(Json).map_err(api_err)
}
