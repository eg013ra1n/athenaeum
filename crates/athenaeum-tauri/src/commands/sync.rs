// Personal-sync commands (Stage I, task A7) — thin wrappers only.
// Business logic lives in `athenaeum_core::api::sync`; the receive-side runtime
// lives in `AppState.sync`. Mirrors `crates/athenaeum-web/src/routes/sync.rs`.

use std::sync::Arc;

use tauri::{AppHandle, State};

use athenaeum_core::api::sync as api;
use athenaeum_core::api::sync::{EnqueueSelectionResult, SyncHistoryQuery};
use athenaeum_core::events::ProgressEmitter;
use athenaeum_core::sync::{HistoryRow, SyncStatus};

use crate::tauri_events::TauriProgressEmitter;
use super::AppState;

/// Dev-flagged: lazily start the receiver + iroh transport and return this
/// device's pairing ticket. The spawned receiver emits `sync-progress` /
/// `sync-finished` through a Tauri event emitter built from `app`.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_sync_pairing_ticket(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let emitter = Arc::new(TauriProgressEmitter(app));
    api::get_pairing_ticket(Arc::clone(&state.ctx), &state.sync, emitter)
        .await
        .map_err(|e| e.to_string())
}

/// Snapshot of the receive side for the Transfers UI.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_sync_status(state: State<'_, AppState>) -> Result<SyncStatus, String> {
    api::get_status(&state.ctx, &state.sync, &state.sync_sender)
        .await
        .map_err(|e| e.to_string())
}

/// The transfer history (received + sent), newest first.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_sync_history(
    state: State<'_, AppState>,
    query: SyncHistoryQuery,
) -> Result<Vec<HistoryRow>, String> {
    api::list_history(&state.ctx, query).map_err(|e| e.to_string())
}

/// Explicit-target send (sync 2C): enqueue the eligible frames of a selection to
/// the chosen destination device as one package. `destination_device_id` is an
/// account device id, resolved to its node id via [`api::resolve_dest_node`].
/// Ineligible frames come back in the result.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn enqueue_sync_selection(
    state: State<'_, AppState>,
    app: AppHandle,
    frame_ids: Vec<i64>,
    destination_device_id: String,
) -> Result<EnqueueSelectionResult, String> {
    // The host emitter, captured into the sender engine on its first spawn, so
    // send-side state transitions surface as `sync-progress`/`sync-finished`.
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(TauriProgressEmitter(app));
    let dest = api::resolve_dest_node(&state.ctx, &destination_device_id)
        .await
        .map_err(|e| e.to_string())?;
    api::enqueue_sync_selection(&state.ctx, &state.sync_sender, dest, frame_ids, Some(emitter))
        .await
        .map_err(|e| e.to_string())
}

/// Whether full-app capture-node auto mode is enabled.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_sync_auto_mode(state: State<'_, AppState>) -> Result<bool, String> {
    api::get_sync_auto_mode(&state.ctx).map_err(|e| e.to_string())
}

/// Toggle full-app capture-node auto mode.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_sync_auto_mode(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    api::set_sync_auto_mode(&state.ctx, enabled).map_err(|e| e.to_string())
}

/// Map of peer node-id-hex → hub device name, for showing friendly names in the
/// transfer history. Best-effort: hub unreachable / signed out → empty map.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_sync_device_names(
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, String>, String> {
    api::get_sync_device_names(&state.ctx).await.map_err(|e| e.to_string())
}
