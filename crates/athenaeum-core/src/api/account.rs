//! Account command handlers (task B4). Thin Tauri/Axum wrappers call these; the
//! logic lives here so both backends stay identical.
//!
//! Composition of the [`crate::account`] building blocks:
//! - [`HubClient`] against `account.hub_url`,
//! - the shared [`DeviceKey`] (== the sync transport identity),
//! - the OS-keychain [`TokenStore`] (token never in DB / logs).
//!
//! Non-secret account state (`account.email` / `account.device_id` /
//! `account.role`) is persisted in the `settings` table so [`status`] resolves
//! **offline**; the device token lives in the keychain only.
//!
//! # Sign-out is local-only
//!
//! [`sign_out`] drops the local token and clears persisted state but does **not**
//! server-revoke this device. Rationale: a network blip during sign-out must
//! never trap the user in a half-signed-out state, and a local token drop is
//! immediate and reliable. The device stays listed on the hub and can be
//! explicitly revoked from any signed-in device via [`revoke_device`]. A `401`
//! from any authed call additionally clears the local session automatically.

use std::path::PathBuf;

use crate::account::{AccountClientError, AccountDevice, AccountStatus, DeviceKey, DeviceRole, HubClient, TokenStore};
use crate::api::{db, ApiError};
use crate::services::ServiceContext;
use crate::settings::{defaults, keys};

/// Resolved, DB-free account configuration for one call.
struct AccountConfig {
    hub_url: String,
    hub_host: String,
    sync_dir: PathBuf,
    account_dir: PathBuf,
}

impl AccountConfig {
    /// Build the [`TokenStore`] for this hub (keychain account = hub host; the
    /// 0600 file fallback lives beside the catalog under `account/`).
    fn token_store(&self) -> TokenStore {
        let file = self.account_dir.join(format!("token_{}", sanitize(&self.hub_host)));
        TokenStore::new(self.hub_host.clone(), file)
    }
}

/// Read `account.hub_url` + resolve the data dirs, dropping the DB borrow before
/// the caller awaits any network I/O.
fn resolve_config(ctx: &ServiceContext) -> Result<AccountConfig, ApiError> {
    let db = db(ctx)?;
    let hub_url = {
        let conn = db.conn();
        ctx.settings
            .get_with_precedence(&conn, keys::ACCOUNT_HUB_URL, defaults::ACCOUNT_HUB_URL)?
    };
    let db_path = db.path().to_path_buf();
    let parent = db_path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    Ok(AccountConfig {
        hub_host: hub_host(&hub_url),
        hub_url,
        // The device key MUST be the transport's key — same `<db_parent>/sync`
        // dir the receiver uses (`api::sync::sync_paths`), one identity.
        sync_dir: parent.join("sync"),
        account_dir: parent.join("account"),
    })
}

/// Extract the host from a hub URL for keychain scoping; falls back to the raw
/// string if it does not parse.
fn hub_host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| url.to_string())
}

/// Filesystem-safe token-file discriminator.
fn sanitize(host: &str) -> String {
    host.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect()
}

/// Best-effort device name for the hub device list. GUI apps on macOS often do
/// not inherit `HOSTNAME`, so this degrades to a generic label rather than
/// failing sign-in — the name is cosmetic and can be renamed later.
fn device_name() -> String {
    for var in ["ATHENAEUM_DEVICE_NAME", "HOSTNAME", "COMPUTERNAME"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim();
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    "Athenaeum".to_string()
}

// ── settings-backed account state ───────────────────────────────────────────

fn read_state(ctx: &ServiceContext, key: &str) -> Result<Option<String>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let v = crate::db::get_setting(&conn, key)?;
    Ok(v.filter(|s| !s.is_empty()))
}

fn write_state(ctx: &ServiceContext, key: &str, value: &str) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    crate::db::set_setting(&conn, key, value)?;
    Ok(())
}

fn clear_state(ctx: &ServiceContext, key: &str) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    crate::db::delete_setting(&conn, key)?;
    Ok(())
}

/// Drop the local token + clear persisted account identity (email / device id /
/// role). Used by [`sign_out`] and on any `401` from the hub.
fn clear_local_session(ctx: &ServiceContext, cfg: &AccountConfig) -> Result<(), ApiError> {
    cfg.token_store()
        .delete()
        .map_err(|e| ApiError::Internal(format!("clear token: {e:#}")))?;
    clear_state(ctx, keys::ACCOUNT_EMAIL)?;
    clear_state(ctx, keys::ACCOUNT_DEVICE_ID)?;
    clear_state(ctx, keys::ACCOUNT_ROLE)?;
    Ok(())
}

/// Map a client error to an `ApiError` carrying an actionable message.
fn map_client_err(e: AccountClientError) -> ApiError {
    match e {
        AccountClientError::RateLimited => {
            ApiError::Invalid("Too many requests — wait a minute and try again.".into())
        }
        AccountClientError::Unauthorized => {
            ApiError::SignedOut("Signed out or device revoked — sign in again.".into())
        }
        AccountClientError::SecondPrimary(m) => ApiError::Conflict(m),
        AccountClientError::PeerValidation(m) => ApiError::Invalid(m),
        AccountClientError::BadRequest(m) => ApiError::Invalid(m),
        AccountClientError::Network(m) => ApiError::Internal(format!("Hub request failed: {m}")),
    }
}

/// As [`map_client_err`], but a `401` first clears the local session so the app
/// reflects the signed-out state on the next [`status`] poll.
fn map_authed_err(ctx: &ServiceContext, cfg: &AccountConfig, e: AccountClientError) -> ApiError {
    if matches!(e, AccountClientError::Unauthorized) {
        if let Err(clear_err) = clear_local_session(ctx, cfg) {
            tracing::warn!(error = %clear_err, "failed to clear local session after 401");
        }
    }
    map_client_err(e)
}

/// Load the local token or fail with a typed signed-out error.
fn require_token(cfg: &AccountConfig) -> Result<String, ApiError> {
    cfg.token_store()
        .load()
        .map_err(|e| ApiError::Internal(format!("load token: {e:#}")))?
        .ok_or_else(|| ApiError::SignedOut("Not signed in.".into()))
}

// ── commands ────────────────────────────────────────────────────────────────

/// Request an email OTP for `email`. Always succeeds when the hub accepts the
/// request (the hub returns 204 whether or not the email exists — no user
/// enumeration).
pub async fn sign_in_start(ctx: &ServiceContext, email: String) -> Result<(), ApiError> {
    let email = email.trim().to_string();
    if email.is_empty() {
        return Err(ApiError::Invalid("Email is required.".into()));
    }
    let cfg = resolve_config(ctx)?;
    let client = HubClient::new(&cfg.hub_url).map_err(map_client_err)?;
    client.request_otp(&email).await.map_err(map_client_err)?;
    tracing::info!(hub = %cfg.hub_host, "otp requested");
    Ok(())
}

/// Verify the OTP, register this device (its shared pubkey) with the hub, store
/// the returned device token in the keychain, and persist account identity.
pub async fn sign_in_verify(
    ctx: &ServiceContext,
    email: String,
    code: String,
) -> Result<AccountStatus, ApiError> {
    let email = email.trim().to_string();
    let code = code.trim().to_string();
    if email.is_empty() || code.is_empty() {
        return Err(ApiError::Invalid("Email and code are required.".into()));
    }
    let cfg = resolve_config(ctx)?;

    // The SAME key the sync transport binds — never a second identity.
    let key = DeviceKey::load_or_create_in(&cfg.sync_dir)
        .map_err(|e| ApiError::Internal(format!("device key: {e:#}")))?;

    let client = HubClient::new(&cfg.hub_url).map_err(map_client_err)?;
    let resp = client
        .verify(&email, &code, &key.pubkey_base64(), &device_name())
        .await
        .map_err(map_client_err)?;

    cfg.token_store()
        .store(&resp.device_token)
        .map_err(|e| ApiError::Internal(format!("store token: {e:#}")))?;

    write_state(ctx, keys::ACCOUNT_EMAIL, &email)?;
    write_state(ctx, keys::ACCOUNT_DEVICE_ID, &resp.device_id)?;
    // A freshly registered device has no role until `set_machine_role`.
    clear_state(ctx, keys::ACCOUNT_ROLE)?;

    tracing::info!(hub = %cfg.hub_host, device_id = %resp.device_id, "device signed in");
    build_status(ctx, &cfg)
}

/// This device's account state — resolvable offline from the keychain + settings.
pub async fn status(ctx: &ServiceContext) -> Result<AccountStatus, ApiError> {
    let cfg = resolve_config(ctx)?;
    build_status(ctx, &cfg)
}

/// Assemble [`AccountStatus`] from local state (no network).
fn build_status(ctx: &ServiceContext, cfg: &AccountConfig) -> Result<AccountStatus, ApiError> {
    let signed_in = cfg
        .token_store()
        .load()
        .map_err(|e| ApiError::Internal(format!("load token: {e:#}")))?
        .is_some();
    let (email, device_id, role) = if signed_in {
        (
            read_state(ctx, keys::ACCOUNT_EMAIL)?,
            read_state(ctx, keys::ACCOUNT_DEVICE_ID)?,
            read_state(ctx, keys::ACCOUNT_ROLE)?.and_then(|s| DeviceRole::parse(&s)),
        )
    } else {
        (None, None, None)
    };
    Ok(AccountStatus {
        signed_in,
        email,
        device_id,
        role,
        hub_url: cfg.hub_url.clone(),
    })
}

/// Local-only sign-out (see module docs). Idempotent.
pub async fn sign_out(ctx: &ServiceContext) -> Result<(), ApiError> {
    let cfg = resolve_config(ctx)?;
    clear_local_session(ctx, &cfg)?;
    tracing::info!(hub = %cfg.hub_host, "signed out (local)");
    Ok(())
}

/// The account's registered devices (fresh from the hub).
pub async fn list_devices(ctx: &ServiceContext) -> Result<Vec<AccountDevice>, ApiError> {
    let cfg = resolve_config(ctx)?;
    let token = require_token(&cfg)?;
    let client = HubClient::new(&cfg.hub_url).map_err(map_client_err)?;
    client
        .list_devices(&token)
        .await
        .map_err(|e| map_authed_err(ctx, &cfg, e))
}

/// Revoke a device by id (this device or a peer).
pub async fn revoke_device(ctx: &ServiceContext, device_id: String) -> Result<(), ApiError> {
    let cfg = resolve_config(ctx)?;
    let token = require_token(&cfg)?;
    let client = HubClient::new(&cfg.hub_url).map_err(map_client_err)?;
    client
        .revoke_device(&token, &device_id)
        .await
        .map_err(|e| map_authed_err(ctx, &cfg, e))?;
    // If we just revoked ourselves, reflect it locally.
    if read_state(ctx, keys::ACCOUNT_DEVICE_ID)?.as_deref() == Some(device_id.as_str()) {
        clear_local_session(ctx, &cfg)?;
    }
    tracing::info!(hub = %cfg.hub_host, device_id = %device_id, "device revoked");
    Ok(())
}

/// Set THIS machine's role. `peer_device_id = None` clears any peer link (the
/// hub rejects a `primary` with a peer, and a missing / cross-account / revoked
/// peer, with a 400 whose message is surfaced).
pub async fn set_machine_role(
    ctx: &ServiceContext,
    role: DeviceRole,
    peer_device_id: Option<String>,
) -> Result<AccountStatus, ApiError> {
    let cfg = resolve_config(ctx)?;
    let token = require_token(&cfg)?;
    let device_id = read_state(ctx, keys::ACCOUNT_DEVICE_ID)?
        .ok_or_else(|| ApiError::SignedOut("Not signed in.".into()))?;

    let client = HubClient::new(&cfg.hub_url).map_err(map_client_err)?;
    client
        .set_role(&token, &device_id, role, peer_device_id.as_deref())
        .await
        .map_err(|e| map_authed_err(ctx, &cfg, e))?;

    write_state(ctx, keys::ACCOUNT_ROLE, role.as_str())?;
    tracing::info!(hub = %cfg.hub_host, device_id = %device_id, role = %role.as_str(), "machine role set");
    build_status(ctx, &cfg)
}
