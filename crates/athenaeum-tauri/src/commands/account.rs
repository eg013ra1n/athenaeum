// Account commands (Stage II, task B4) — thin wrappers only.
// Business logic lives in `athenaeum_core::api::account`; mirrors the Axum
// routes in `crates/athenaeum-web/src/routes/account.rs`.

use tauri::State;

use athenaeum_core::account::{AccountDevice, AccountStatus};
use athenaeum_core::api::account as api;

use super::AppState;

/// Request an email OTP for `email`.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn account_sign_in_start(state: State<'_, AppState>, email: String) -> Result<(), String> {
    api::sign_in_start(&state.ctx, email).await.map_err(|e| e.to_string())
}

/// Verify the OTP, register this device with the hub, and store its token.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn account_sign_in_verify(
    state: State<'_, AppState>,
    email: String,
    code: String,
) -> Result<AccountStatus, String> {
    api::sign_in_verify(&state.ctx, email, code)
        .await
        .map_err(|e| e.to_string())
}

/// This device's account state (offline-resolvable).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn account_status(state: State<'_, AppState>) -> Result<AccountStatus, String> {
    api::status(&state.ctx).await.map_err(|e| e.to_string())
}

/// Local-only sign-out (drops the token, clears persisted account state).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn account_sign_out(state: State<'_, AppState>) -> Result<(), String> {
    api::sign_out(&state.ctx).await.map_err(|e| e.to_string())
}

/// The account's registered devices (fresh from the hub).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_account_devices(state: State<'_, AppState>) -> Result<Vec<AccountDevice>, String> {
    api::list_devices(&state.ctx).await.map_err(|e| e.to_string())
}

/// Revoke a device by id.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn revoke_account_device(
    state: State<'_, AppState>,
    device_id: String,
) -> Result<(), String> {
    api::revoke_device(&state.ctx, device_id)
        .await
        .map_err(|e| e.to_string())
}

/// Rename a device by id (this device or a peer). Returns the refreshed status.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn rename_device(
    state: State<'_, AppState>,
    device_id: String,
    name: String,
) -> Result<AccountStatus, String> {
    api::rename_device(&state.ctx, device_id, name)
        .await
        .map_err(|e| e.to_string())
}
