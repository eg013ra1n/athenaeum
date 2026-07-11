// Account route handlers (Stage II, task B4) — thin wrappers only.
// Business logic lives in `athenaeum_core::api::account`; mirrors the Tauri
// commands in `crates/athenaeum-tauri/src/commands/account.rs`.

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use athenaeum_core::account::{AccountDevice, AccountStatus};
use athenaeum_core::api::account as api;

use crate::routes::api_err;
use crate::WebAppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignInStartReq {
    email: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignInVerifyReq {
    email: String,
    code: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevokeDeviceReq {
    device_id: String,
}

/// POST /api/account_sign_in_start
#[tracing::instrument(skip_all, err(Debug))]
pub async fn account_sign_in_start(
    State(state): State<WebAppState>,
    Json(req): Json<SignInStartReq>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::sign_in_start(&state.ctx, req.email)
        .await
        .map(Json)
        .map_err(api_err)
}

/// POST /api/account_sign_in_verify
#[tracing::instrument(skip_all, err(Debug))]
pub async fn account_sign_in_verify(
    State(state): State<WebAppState>,
    Json(req): Json<SignInVerifyReq>,
) -> Result<Json<AccountStatus>, (StatusCode, String)> {
    api::sign_in_verify(&state.ctx, req.email, req.code)
        .await
        .map(Json)
        .map_err(api_err)
}

/// POST /api/account_status
#[tracing::instrument(skip_all, err(Debug))]
pub async fn account_status(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<AccountStatus>, (StatusCode, String)> {
    api::status(&state.ctx).await.map(Json).map_err(api_err)
}

/// POST /api/account_sign_out
#[tracing::instrument(skip_all, err(Debug))]
pub async fn account_sign_out(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::sign_out(&state.ctx).await.map(Json).map_err(api_err)
}

/// POST /api/list_account_devices
#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_account_devices(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<Vec<AccountDevice>>, (StatusCode, String)> {
    api::list_devices(&state.ctx).await.map(Json).map_err(api_err)
}

/// POST /api/revoke_account_device
#[tracing::instrument(skip_all, err(Debug))]
pub async fn revoke_account_device(
    State(state): State<WebAppState>,
    Json(req): Json<RevokeDeviceReq>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::revoke_device(&state.ctx, req.device_id)
        .await
        .map(Json)
        .map_err(api_err)
}
