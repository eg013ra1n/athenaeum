// Personal-sync commands (Stage I, task A7) — thin wrappers only.
// Business logic lives in `athenaeum_core::api::sync`; the receive-side runtime
// lives in `AppState.sync`. Mirrors `crates/athenaeum-web/src/routes/sync.rs`.

use std::sync::Arc;

use tauri::{AppHandle, State};

use athenaeum_core::api::sync as api;
use athenaeum_core::api::sync::{EnqueueSelectionResult, SyncHistoryQuery};
use athenaeum_core::events::ProgressEmitter;
use athenaeum_core::monitor::ScanCompletionHook;
use athenaeum_core::services::ServiceContext;
use athenaeum_core::sync::{HistoryRow, SyncSenderRuntime, SyncStatus};

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
    api::get_pairing_ticket(&state.ctx, &state.sync, emitter)
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

/// Manual send (task M2): enqueue the eligible frames of a selection to the
/// paired primary as one package. Ineligible frames come back in the result.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn enqueue_sync_selection(
    state: State<'_, AppState>,
    app: AppHandle,
    frame_ids: Vec<i64>,
) -> Result<EnqueueSelectionResult, String> {
    // The host emitter, captured into the sender engine on its first spawn, so
    // send-side state transitions surface as `sync-progress`/`sync-finished`.
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(TauriProgressEmitter(app));
    api::enqueue_sync_selection(&state.ctx, &state.sync_sender, frame_ids, Some(emitter))
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

/// The desktop-side [`ScanCompletionHook`] (task M2 review finding): the
/// background `MonitorService` lives in `athenaeum-core` with only a
/// `ServiceContext`, but the personal-sync sender runtime (`AppState.sync_sender`)
/// is host state — this closes over both and is installed once at startup via
/// `state.monitor.set_scan_completion_hook(...)` (see `lib.rs`), so a
/// monitor-triggered (unattended) scan auto-enqueues exactly like an
/// interactive one. Auto-mode guards (role/signed-in/toggle) are NOT decided
/// here — they live inside `auto_enqueue_scanned_files`, read fresh every fire.
pub struct DesktopScanCompletionHook {
    pub ctx: Arc<ServiceContext>,
    pub sender: Arc<SyncSenderRuntime>,
    /// Host emitter captured into the sender engine on its first spawn so an
    /// unattended (monitor-triggered) auto-enqueue also emits transfer events.
    pub emitter: Arc<dyn ProgressEmitter>,
}

impl ScanCompletionHook for DesktopScanCompletionHook {
    fn on_scan_completed(&self, new_file_ids: Vec<i64>) {
        let ctx = Arc::clone(&self.ctx);
        let sender = Arc::clone(&self.sender);
        let emitter = Arc::clone(&self.emitter);
        tauri::async_runtime::spawn(async move {
            if let Err(e) =
                api::auto_enqueue_scanned_files(&ctx, &sender, new_file_ids, Some(emitter)).await
            {
                tracing::warn!(error = %e, "auto-mode sync enqueue after monitor scan failed");
            }
        });
    }
}
