//! Perseus web account sign-in — the `/api/account/*` endpoints (Task 5).
//!
//! The Account section of the status page drives the hub's email→OTP sign-in
//! through these four handlers. All of them are registered **inside** the
//! bearer-gated `api` router in [`super::build_router`] (never auth-exempt — the
//! only exemption is `GET /`, the static shell). The real work lives in the
//! non-interactive [`crate::account`] core (Task 2); these handlers are thin
//! adapters that snapshot the live config, call it, and — on a successful
//! sign-in / sign-out — ring [`WebState::supervisor_wake`] so the supervisor
//! re-reads readiness and starts/stops the sync engine at once instead of on its
//! next poll.
//!
//! # Error surface
//!
//! A hub failure passes through **honestly**: the anyhow chain is logged
//! (`tracing::error!`, never swallowed) and returned as `502 BAD_GATEWAY` with
//! that same text as the body, so the page can show what actually went wrong. A
//! failed verify stores nothing, so `signedIn` stays `false`. The OTP `code` and
//! the device token are NEVER logged or echoed — the code lives only in the
//! request body, and the token never leaves the 0600 file store.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Json};

use super::WebState;

/// `GET /api/account` payload: the signed-in snapshot for the Account card.
///
/// A field-for-field projection of [`crate::account::AccountStatus`]. All
/// identity fields are display-only; `signedIn` is the authoritative gate (a
/// stored device token). No credential (token) is ever part of this DTO.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountDto {
    signed_in: bool,
    email: Option<String>,
    hub_url: Option<String>,
    device_id: Option<String>,
    primary_device_id: Option<String>,
    primary_name: Option<String>,
}

impl From<crate::account::AccountStatus> for AccountDto {
    fn from(s: crate::account::AccountStatus) -> Self {
        Self {
            signed_in: s.signed_in,
            email: s.email,
            hub_url: s.hub_url,
            device_id: s.device_id,
            primary_device_id: s.primary_device_id,
            primary_name: s.primary_name,
        }
    }
}

/// `GET /api/account` — the current signed-in snapshot. Pure: reads the config +
/// pairing cache, makes no hub call, so the page can poll it cheaply.
pub async fn api_account_get(State(state): State<Arc<WebState>>) -> Json<AccountDto> {
    let config = state.config.read().await.clone();
    Json(crate::account::account_status(&config).into())
}

/// `POST /api/account/request-code` request body.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestCodeBody {
    email: String,
}

/// `POST /api/account/request-code` — ask the hub to email a one-time code for
/// `email`. A hub failure passes through as `502` with the anyhow chain text.
pub async fn api_account_request_code(
    State(state): State<Arc<WebState>>,
    Json(body): Json<RequestCodeBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let config = state.config.read().await.clone();
    crate::account::request_code(&config, &body.email)
        .await
        .map_err(|e| {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, "request-code failed");
            (StatusCode::BAD_GATEWAY, msg)
        })?;
    Ok(StatusCode::OK)
}

/// `POST /api/account/verify` request body. The `code` is never logged.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyBody {
    email: String,
    code: String,
}

/// `POST /api/account/verify` — verify the OTP, store the device token, register
/// this device as a capture device, refresh the friendly-name cache for history
/// rows, and wake the supervisor so it launches the engine now. Returns the
/// fresh [`AccountDto`]. A hub failure passes through as `502` and stores
/// nothing (`signedIn` stays `false`).
pub async fn api_account_verify(
    State(state): State<Arc<WebState>>,
    Json(body): Json<VerifyBody>,
) -> Result<Json<AccountDto>, (StatusCode, String)> {
    let config = state.config.read().await.clone();
    crate::account::web_sign_in(&config, &body.email, &body.code)
        .await
        .map_err(|e| {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, "sign-in failed");
            (StatusCode::BAD_GATEWAY, msg)
        })?;
    // Refresh the friendly-name cache (history rows resolve peer hex → name from
    // it), then prod the supervisor into an immediate readiness re-read so the
    // sync engine comes up without waiting for its next poll.
    *state.device_names.write().await =
        crate::account::PairingCache::load(&config.data_dir).device_names;
    state.supervisor_wake.notify_one();
    Ok(Json(crate::account::account_status(&config).into()))
}

/// `POST /api/account/logout` — delete the stored token and reset the pairing
/// cache (idempotent via [`crate::account::sign_out`]), then wake the supervisor
/// so it stops the engine. A filesystem error passes through as `500`.
pub async fn api_account_logout(
    State(state): State<Arc<WebState>>,
) -> Result<StatusCode, (StatusCode, String)> {
    let config = state.config.read().await.clone();
    crate::account::sign_out(&config).map_err(|e| {
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "sign-out failed");
        (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;
    state.supervisor_wake.notify_one();
    Ok(StatusCode::OK)
}
