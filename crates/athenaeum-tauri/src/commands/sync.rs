// Personal-sync commands (Stage I, task A7) — thin wrappers only.
// Business logic lives in `athenaeum_core::api::sync`; the receive-side runtime
// lives in `AppState.sync`. Mirrors `crates/athenaeum-web/src/routes/sync.rs`.

use std::sync::Arc;

use tauri::{AppHandle, State};

use athenaeum_core::api::lights::{FlatNormMode, LightCalParams};
use athenaeum_core::api::sync as api;
use athenaeum_core::api::sync::{
    DeletedTransferRecord, EnqueueSelectionResult, SyncHistoryQuery, TerminalTransfers,
    TransferCleanup, TransferEventEntry, TransferStorage,
};
use athenaeum_core::events::ProgressEmitter;
use athenaeum_core::export::models::ExportMode;
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
        Arc::clone(&state.sync),
        Arc::clone(&state.sync_sender),
        Arc::clone(&state.collab_sender),
        emitter,
    )
    .await
    .map_err(|e| e.to_string())
}

/// Snapshot of the receive side for the Transfers UI. Instrumented at `debug`
/// (hot-path rule, `get_setting`/`get_frame_preview` precedent): the Transfers UI
/// polls this every 10s, so an `info` boundary span would flood the logs.
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
pub async fn get_sync_status(state: State<'_, AppState>) -> Result<SyncStatus, String> {
    api::get_status(&state.ctx, &state.sync, &state.sync_sender)
        .await
        .map_err(|e| e.to_string())
}

/// The transfer history (received + sent), newest first.
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
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
#[tracing::instrument(skip_all, err, level = "debug")]
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
#[tracing::instrument(skip_all, err, level = "debug")]
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
#[tracing::instrument(skip_all, err, level = "debug")]
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

/// Frame-set send from the Export tab (spec 2026-08-28): one package per
/// destination holding what the chosen export mode would put on disk.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_frame_set_send(
    state: State<'_, AppState>,
    app: AppHandle,
    frame_set_id: i64,
    mode: ExportMode,
    destination_device_id: String,
    batch_name: Option<String>,
    flat_norm: Option<bool>,
    flat_norm_mode: Option<FlatNormMode>,
    params: Option<LightCalParams>,
) -> Result<EnqueueSelectionResult, String> {
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(TauriProgressEmitter(app));
    let dest = api::resolve_dest_node(&state.ctx, &destination_device_id)
        .await
        .map_err(|e| e.to_string())?;
    api::enqueue_frame_set_send(
        &state.ctx,
        &state.sync_sender,
        Arc::clone(&state.collab_sender),
        &state.sync,
        dest,
        frame_set_id,
        mode,
        batch_name,
        flat_norm.unwrap_or(true),
        flat_norm_mode.unwrap_or(FlatNormMode::CentralThird),
        params.unwrap_or_default(),
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

/// Delete one transfer batch's durable records (Transfers Status Model v2 UX
/// wave 2): removes the batch's state row(s), per-file rows, event journal, and
/// history rows. Refuses (`Invalid`) if any attempt is still active. Records only
/// — never the received files on disk or the catalog.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_transfer_history(
    state: State<'_, AppState>,
    direction: Direction,
    package_key: String,
) -> Result<DeletedTransferRecord, String> {
    api::delete_transfer_history(&state.ctx, &state.sync, direction, package_key)
        .await
        .map_err(|e| e.to_string())
}

/// On-disk footprint of the transfer temp data (Batch Model §D4, B7): package
/// payload dirs + the blob store dir, for the Settings "Transfer storage" line.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_transfer_storage(state: State<'_, AppState>) -> Result<TransferStorage, String> {
    api::get_transfer_storage(&state.ctx).map_err(|e| e.to_string())
}

/// Reclaim finished transfers' temp data on demand (Batch Model §D4, B7): remove
/// terminal outbound rows' payload dirs and release orphan receiver in-flight blob
/// tags. Records untouched — reclaims disk only.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cleanup_finished_transfers(
    state: State<'_, AppState>,
) -> Result<TransferCleanup, String> {
    api::cleanup_finished_transfers(&state.ctx, &state.sync)
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

/// Persist and live-apply the device-wide sync UPLOAD limit in bytes/sec
/// (W1). `0` = unlimited; any real cap must be >= 100000. Reads go through the
/// generic `get_setting` — there is no dedicated getter command.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_sync_upload_limit(
    state: State<'_, AppState>,
    bytes_per_sec: u64,
) -> Result<(), String> {
    api::set_sync_upload_limit(&state.ctx, bytes_per_sec)
        .await
        .map_err(|e| e.to_string())
}

/// Persist and live-apply the cap on simultaneous INCOMING transfers (W2 T2.7),
/// 1..=8. Resizes the running receiver's gate without restarting it (a shrink
/// lands as the in-flight lanes finish) and is re-read at the next receiver
/// start. Reads go through the generic `get_setting` — there is no dedicated
/// getter command.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_sync_max_concurrent_receives(
    state: State<'_, AppState>,
    max_concurrent_receives: usize,
) -> Result<(), String> {
    api::set_sync_max_concurrent_receives(&state.ctx, &state.sync, max_concurrent_receives)
        .await
        .map_err(|e| e.to_string())
}

/// Map of peer node-id-hex → hub device name, for showing friendly names in the
/// transfer history. Best-effort: hub unreachable / signed out → empty map.
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
pub async fn get_sync_device_names(
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, String>, String> {
    api::get_sync_device_names(&state.ctx).await.map_err(|e| e.to_string())
}

/// Map of peer node-id-hex → device capability (`"athenaeum"` / `"perseus"`),
/// for the Transfers UI's per-transfer origin badge. Best-effort: hub
/// unreachable / signed out → empty map.
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
pub async fn get_sync_device_capabilities(
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, String>, String> {
    api::get_sync_device_capabilities(&state.ctx).await.map_err(|e| e.to_string())
}
