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
    clear_state(ctx, keys::ACCOUNT_PEER_DEVICE_ID)?;
    clear_sync_caches(ctx)?;
    Ok(())
}

/// Clear the sync pairing caches (peer + relay map; task M1 review minor (a)).
/// A signed-out device must not keep offering a stale peer/relay-map cache to
/// the next sign-in's resolution. Extracted into its own function so it is
/// unit-testable without exercising [`clear_local_session`]'s keychain call.
fn clear_sync_caches(ctx: &ServiceContext) -> Result<(), ApiError> {
    clear_state(ctx, keys::SYNC_CACHED_PEER)?;
    clear_state(ctx, keys::SYNC_CACHED_RELAYS)?;
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
        AccountClientError::DeviceConflict(m) => ApiError::Conflict(m),
        AccountClientError::PeerValidation(m) => ApiError::Invalid(m),
        AccountClientError::BadRequest(m) => ApiError::Invalid(m),
        AccountClientError::Network(m) => ApiError::Internal(format!("Hub request failed: {m}")),
    }
}

/// As [`map_client_err`], but tuned for the `/auth/verify` response: a 401
/// there means "wrong or expired code" — the caller is mid sign-in, not
/// signed-out-and-revoked, so the generic [`ApiError::SignedOut`] message
/// would be actively misleading (there is no session to be "out" of yet).
fn map_verify_err(e: AccountClientError) -> ApiError {
    match e {
        AccountClientError::Unauthorized => ApiError::Invalid("Wrong or expired code.".into()),
        other => map_client_err(other),
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
        .map_err(map_verify_err)?;

    cfg.token_store()
        .store(&resp.device_token)
        .map_err(|e| ApiError::Internal(format!("store token: {e:#}")))?;

    write_state(ctx, keys::ACCOUNT_EMAIL, &email)?;
    write_state(ctx, keys::ACCOUNT_DEVICE_ID, &resp.device_id)?;
    // A freshly registered device has no role until `set_machine_role`. But on a
    // plain *re-sign-in* the hub still holds this device's role + peer from before
    // (the token/pubkey are unchanged) — clearing them locally here would drop the
    // capture pairing until the next manual role set. Refresh from the hub
    // (best-effort, one extra call) so the persisted role/peer match the hub
    // again; a fresh device simply gets nothing back and stays unassigned.
    clear_state(ctx, keys::ACCOUNT_ROLE)?;
    clear_state(ctx, keys::ACCOUNT_PEER_DEVICE_ID)?;
    refresh_persisted_role(ctx, &cfg, &resp.device_id, &resp.device_token).await;

    tracing::info!(hub = %cfg.hub_host, device_id = %resp.device_id, "device signed in");
    build_status(ctx, &cfg)
}

/// Best-effort: re-read this device's hub row and persist its current `role` +
/// `peer_device_id` locally. Called after a successful sign-in/verify so a
/// re-sign-in restores the role the hub already holds (the known B4 gap). Never
/// fails sign-in — a hub hiccup just leaves the role unassigned until the next
/// [`list_devices`] / [`set_machine_role`].
async fn refresh_persisted_role(
    ctx: &ServiceContext,
    cfg: &AccountConfig,
    device_id: &str,
    token: &str,
) {
    let client = match HubClient::new(&cfg.hub_url) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "role refresh: could not build hub client");
            return;
        }
    };
    let devices = match client.list_devices(token).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "role refresh: list_devices failed; role left unassigned");
            return;
        }
    };
    let Some(me) = devices.iter().find(|d| d.id == device_id) else {
        return;
    };
    if let Some(role) = me.role {
        if let Err(e) = write_state(ctx, keys::ACCOUNT_ROLE, role.as_str()) {
            tracing::warn!(error = %e, "role refresh: persist role failed");
        }
    }
    match me.peer_device_id.as_deref() {
        Some(peer) => {
            if let Err(e) = write_state(ctx, keys::ACCOUNT_PEER_DEVICE_ID, peer) {
                tracing::warn!(error = %e, "role refresh: persist peer failed");
            }
        }
        None => {
            let _ = clear_state(ctx, keys::ACCOUNT_PEER_DEVICE_ID);
        }
    }
    tracing::info!(device_id = %device_id, "persisted role/peer refreshed from hub after sign-in");
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
    // Persist the paired peer so the sync peer-resolver (task M1) can look up the
    // primary's pubkey; a `primary`/unassigned role or a cleared peer wipes it.
    match (role, peer_device_id.as_deref()) {
        (DeviceRole::Capture, Some(peer)) => write_state(ctx, keys::ACCOUNT_PEER_DEVICE_ID, peer)?,
        _ => clear_state(ctx, keys::ACCOUNT_PEER_DEVICE_ID)?,
    }
    tracing::info!(hub = %cfg.hub_host, device_id = %device_id, role = %role.as_str(), "machine role set");
    build_status(ctx, &cfg)
}

// ── sync peer/relay resolution seam (task M1) ───────────────────────────────
//
// The app's capture-role sender resolves its peer + relays through
// `crate::sync::pairing`, exactly like Perseus. These helpers gather the
// settings/keychain inputs that resolver needs; they live here (not in
// `api::sync`) because they read the same `AccountConfig` + `TokenStore` the
// account commands use.

/// The signed-in hub credentials `(hub_url, token)`, or `None` when signed out.
/// Used to fetch the relay map for the sync transport.
pub fn hub_credentials(ctx: &ServiceContext) -> Result<Option<(String, String)>, ApiError> {
    let cfg = resolve_config(ctx)?;
    let token = cfg
        .token_store()
        .load()
        .map_err(|e| ApiError::Internal(format!("load token: {e:#}")))?;
    Ok(token.map(|t| (cfg.hub_url.clone(), t)))
}

/// The account-pairing inputs for the sync peer resolver: `Some` only when this
/// device is signed in, has role `capture`, and has a persisted paired primary.
/// `None` otherwise (the resolver then falls through to the dev-ticket path).
pub fn account_pairing(
    ctx: &ServiceContext,
) -> Result<Option<crate::sync::AccountPairing>, ApiError> {
    let cfg = resolve_config(ctx)?;
    let Some(token) = cfg
        .token_store()
        .load()
        .map_err(|e| ApiError::Internal(format!("load token: {e:#}")))?
    else {
        return Ok(None);
    };
    let role = read_state(ctx, keys::ACCOUNT_ROLE)?.and_then(|s| DeviceRole::parse(&s));
    if role != Some(DeviceRole::Capture) {
        return Ok(None);
    }
    let Some(peer_device_id) = read_state(ctx, keys::ACCOUNT_PEER_DEVICE_ID)? else {
        return Ok(None);
    };
    Ok(Some(crate::sync::AccountPairing {
        hub_url: cfg.hub_url,
        token,
        peer_device_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fix (B4 review, minor #1): a wrong/expired OTP code must read as a
    /// code problem, not "you're signed out" — the user is mid sign-in, there
    /// is no session to be out of. Guards against `map_verify_err` regressing
    /// back to a bare `map_client_err` delegation (its pre-fix RED state).
    #[test]
    fn verify_401_maps_to_wrong_code_message_not_signed_out() {
        let err = map_verify_err(AccountClientError::Unauthorized);
        match err {
            ApiError::Invalid(msg) => {
                assert!(
                    msg.to_lowercase().contains("code"),
                    "verify-401 message should mention the code, got: {msg}"
                );
            }
            other => panic!("expected ApiError::Invalid (wrong-code), got {other:?}"),
        }
    }

    /// The generic (non-verify) 401 mapping is unchanged: any OTHER authed call
    /// (list_devices/revoke/set_role) still reads as SignedOut.
    #[test]
    fn generic_401_still_maps_to_signed_out() {
        assert!(matches!(map_client_err(AccountClientError::Unauthorized), ApiError::SignedOut(_)));
    }

    /// Cross-account pubkey conflict on verify still surfaces as a conflict,
    /// carrying the hub's message.
    #[test]
    fn verify_409_maps_to_conflict_with_message() {
        let err = map_verify_err(AccountClientError::DeviceConflict("device public key already registered".into()));
        match err {
            ApiError::Conflict(msg) => assert_eq!(msg, "device public key already registered"),
            other => panic!("expected ApiError::Conflict, got {other:?}"),
        }
    }

    /// Task M1 review minor (a): sign-out (via `clear_sync_caches`, the piece of
    /// `clear_local_session` that doesn't touch the keychain) must also drop the
    /// sync pairing caches — a signed-out device must not keep offering a stale
    /// peer/relay-map cache to whatever signs in next.
    #[test]
    fn clear_sync_caches_removes_peer_and_relay_settings() {
        use crate::cache::MemoryImageCache;
        use crate::services::compute_queue::ComputeQueue;
        use crate::services::operation_queue::OperationQueue;
        use crate::settings::SettingsManager;
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex, OnceLock};
        #[cfg(all(feature = "render", feature = "solver"))]
        use std::sync::RwLock;

        let tmp = tempfile::tempdir().unwrap();
        let database = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        {
            let conn = database.conn();
            crate::db::set_setting(&conn, keys::SYNC_CACHED_PEER, &"aa".repeat(32)).unwrap();
            crate::db::set_setting(&conn, keys::SYNC_CACHED_RELAYS, "https://relay1.example.org").unwrap();
        }
        let db_cell = OnceLock::new();
        let _ = db_cell.set(database);
        let ctx = ServiceContext {
            db: db_cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            active_light_cal: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(all(feature = "render", feature = "solver"))]
            dso_catalog: Arc::new(RwLock::new(None)),
            #[cfg(feature = "solver")]
            star_cache: Arc::new(RwLock::new(None)),
            #[cfg(feature = "solver")]
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
        };

        clear_sync_caches(&ctx).unwrap();

        let conn = db(&ctx).unwrap().conn();
        assert_eq!(
            crate::db::get_setting(&conn, keys::SYNC_CACHED_PEER).unwrap(),
            None,
            "the cached peer setting must be gone"
        );
        assert_eq!(
            crate::db::get_setting(&conn, keys::SYNC_CACHED_RELAYS).unwrap(),
            None,
            "the cached relay map setting must be gone"
        );
    }
}
