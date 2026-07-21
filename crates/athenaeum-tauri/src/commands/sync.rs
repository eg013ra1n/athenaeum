// Personal-sync commands (Stage I, task A7) — thin wrappers only.
// Business logic lives in `athenaeum_core::api::sync`; the receive-side runtime
// lives in `AppState.sync`. Mirrors `crates/athenaeum-web/src/routes/sync.rs`.

use std::sync::Arc;

use tauri::{AppHandle, State};

use athenaeum_core::api::sync as api;
use athenaeum_core::api::sync::{
    EnqueueSelectionResult, SyncHistoryQuery, TerminalTransfers, TransferEventEntry,
};
use athenaeum_core::events::ProgressEmitter;
use athenaeum_core::sync::{Direction, HistoryRow, SyncStatus, TransferFileEntry};

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
    api::get_pairing_ticket(
        Arc::clone(&state.ctx),
        &state.sync,
        Arc::clone(&state.sync_sender),
        Arc::clone(&state.collab_sender),
        emitter,
    )
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

/// The recent window of terminal (settled) transfers — sent
/// (confirmed/failed/cancelled) + received (done/failed/cancelled) — that the
/// cheap `get_sync_status` poll omits (tv2 follow-up). The Transfers UI fetches
/// this on mount and on each `sync-finished` so a settled row (and its Resend
/// affordance + detail) survives a restart. `limit` defaults to 100, capped 500.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_terminal_transfers(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> Result<TerminalTransfers, String> {
    api::list_terminal_transfers(&state.ctx, limit).map_err(|e| e.to_string())
}

/// Per-file detail for one transfer batch (Task 14): the outbound (`sent`) or
/// inbound (`received`) package's manifest joined to this node's per-frame
/// verdicts, for the Transfers UI's expand-a-row detail view.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_transfer_files(
    state: State<'_, AppState>,
    direction: Direction,
    id: i64,
) -> Result<Vec<TransferFileEntry>, String> {
    api::list_transfer_files(&state.ctx, direction, id).map_err(|e| e.to_string())
}

/// The event journal for one transfer batch (Transfers Status Model v2 §D7),
/// newest-first — the detail pane's Log tab. Fired on detail-pane open.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_transfer_events(
    state: State<'_, AppState>,
    direction: Direction,
    id: i64,
) -> Result<Vec<TransferEventEntry>, String> {
    api::list_transfer_events(&state.ctx, direction, id).map_err(|e| e.to_string())
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
    batch_name: Option<String>,
    frame_set_id: Option<i64>,
) -> Result<EnqueueSelectionResult, String> {
    // The host emitter, captured into the sender engine on its first spawn, so
    // send-side state transitions surface as `sync-progress`/`sync-finished`.
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(TauriProgressEmitter(app));
    let dest = api::resolve_dest_node(&state.ctx, &destination_device_id)
        .await
        .map_err(|e| e.to_string())?;
    api::enqueue_sync_selection(
        &state.ctx,
        &state.sync_sender,
        Arc::clone(&state.collab_sender),
        &state.sync,
        dest,
        frame_ids,
        batch_name,
        frame_set_id,
        Some(emitter),
    )
    .await
    .map_err(|e| e.to_string())
}

/// Retry a terminal (failed / cancelled) outbound package: re-enqueue its dir as
/// a new durable row on the engine for the original row's peer (built lazily if
/// that peer has no engine yet). Returns the new row id.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn retry_sync_package(
    state: State<'_, AppState>,
    app: AppHandle,
    id: i64,
) -> Result<i64, String> {
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(TauriProgressEmitter(app));
    api::retry_sync_package(
        &state.ctx,
        &state.sync_sender,
        Arc::clone(&state.collab_sender),
        &state.sync,
        id,
        Some(emitter),
    )
    .await
    .map_err(|e| e.to_string())
}

/// Send-now a live outbound package: kick its owning engine so it re-announces
/// immediately instead of waiting out its backoff.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn send_now_sync_package(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    api::send_now_sync_package(&state.sync_sender, id).await.map_err(|e| e.to_string())
}

/// Cancel a live outbound package: drive it to the terminal `Cancelled` state on
/// its owning engine.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_sync_package(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    api::cancel_sync_package(&state.sync_sender, id).await.map_err(|e| e.to_string())
}

/// Cancel an inbound package the receiver is about to fetch or is fetching
/// (Task 12): signals the running receiver so an in-flight fetch aborts and the
/// receiver acks every frame `Cancelled`, then stamps the inbound row `Cancelled`.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_incoming_package(
    state: State<'_, AppState>,
    package_id: String,
) -> Result<(), String> {
    api::cancel_incoming_package(&state.ctx, &state.sync, &package_id)
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
