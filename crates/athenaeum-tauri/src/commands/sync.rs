// Personal-sync commands (Stage I, task A7) — thin wrappers only.
// Business logic lives in `athenaeum_core::api::sync`; the receive-side runtime
// lives in `AppState.sync`. Mirrors `crates/athenaeum-web/src/routes/sync.rs`.

use std::sync::Arc;

use tauri::{AppHandle, State};

use athenaeum_core::api::sync as api;
use athenaeum_core::api::sync::SyncHistoryQuery;
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
    api::get_pairing_ticket(&state.ctx, &state.sync, emitter)
        .await
        .map_err(|e| e.to_string())
}

/// Snapshot of the receive side for the Transfers UI.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_sync_status(state: State<'_, AppState>) -> Result<SyncStatus, String> {
    api::get_status(&state.ctx, &state.sync)
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
