//! Personal-sync command handlers (Stage I, task A7). Thin Tauri/Axum wrappers
//! call these; the business logic lives here so both backends stay identical.
//!
//! Three commands:
//! - [`get_pairing_ticket`] — dev-flagged. Lazily starts the iroh transport +
//!   [`SyncReceiver`](crate::sync::SyncReceiver) and returns this device's
//!   pairing ticket for a peer (e.g. Perseus) to dial.
//! - [`get_status`] — a snapshot for the Transfers UI (dev flag, whether the
//!   transport is up, the ticket, and how many frames have been received).
//! - [`list_history`] — the received/sent transfer log.
//!
//! The transport lifecycle lives in a [`SyncRuntime`](crate::sync::SyncRuntime)
//! held by the **host** `AppState` (desktop + web), passed in explicitly rather
//! than through `ServiceContext` — the receiver needs an
//! [`Arc<dyn ProgressEmitter>`] built from the host (Tauri `AppHandle` /
//! SSE sender), which `ServiceContext` does not carry.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::api::{db, ApiError};
use crate::events::ProgressEmitter;
use crate::services::ServiceContext;
use crate::settings::{defaults, keys};
use crate::sync::store::search_history_rows;
use crate::sync::{
    pairing, HistoryQuery, HistoryRow, PeerResolution, SyncRuntime, SyncStatus,
};

/// Request filter for [`list_history`] (mirrors [`HistoryQuery`] over the
/// command boundary with a JS-friendly shape).
#[derive(Debug, Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SyncHistoryQuery {
    /// Exact `filename` filter (unfiltered when absent).
    pub filename: Option<String>,
    /// Exact `object` filter (unfiltered when absent).
    pub object: Option<String>,
    /// Newest-first cap. `0` is treated as the default cap.
    pub limit: u32,
}

/// Default `list_history` cap when the caller passes `limit = 0`.
const DEFAULT_HISTORY_LIMIT: u32 = 200;

/// Whether the dev ticket-pairing flag is enabled.
fn dev_pairing_enabled(ctx: &ServiceContext) -> Result<bool, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let v = ctx
        .settings
        .get_with_precedence(&conn, keys::SYNC_DEV_TICKET_PAIRING, defaults::SYNC_DEV_TICKET_PAIRING)?;
    Ok(v.eq_ignore_ascii_case("true"))
}

/// Resolve the sync data dir (`<db_parent>/sync`) and the catalog DB path.
/// Everything sync needs — device key, blob store, landed files — lives beside
/// the catalog so it follows the same OS-appdata / Docker `/data` location.
fn sync_paths(ctx: &ServiceContext) -> Result<(PathBuf, PathBuf), ApiError> {
    let db = db(ctx)?;
    let db_path = db.path().to_path_buf();
    let sync_dir = db_path
        .parent()
        .map(|p| p.join("sync"))
        .unwrap_or_else(|| PathBuf::from("sync"));
    Ok((sync_dir, db_path))
}

// ── pairing cache (task M1) ──────────────────────────────────────────────────

/// Read the cached relay map (newline-separated URLs) from settings.
fn cached_relays(ctx: &ServiceContext) -> Result<Vec<String>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let raw = crate::db::get_setting(&conn, keys::SYNC_CACHED_RELAYS)?.unwrap_or_default();
    Ok(raw.lines().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
}

/// Persist a freshly-resolved relay map (best-effort; a failure only weakens the
/// next offline start).
fn store_cached_relays(ctx: &ServiceContext, urls: &[String]) {
    if let Ok(db) = db(ctx) {
        let conn = db.conn();
        if let Err(e) = crate::db::set_setting(&conn, keys::SYNC_CACHED_RELAYS, &urls.join("\n")) {
            tracing::warn!(error = %e, "failed to cache relay map");
        }
    }
}

/// The last cached peer node id, decoded from its 64-char hex form.
fn cached_peer(ctx: &ServiceContext) -> Result<Option<crate::sharing::types::NodeId>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let Some(hex) = crate::db::get_setting(&conn, keys::SYNC_CACHED_PEER)?.filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    Ok(crate::sync::node_id_from_hex(&hex).ok())
}

/// Persist a freshly-resolved peer node id (best-effort).
fn store_cached_peer(ctx: &ServiceContext, peer: &crate::sharing::types::NodeId) {
    if let Ok(db) = db(ctx) {
        let conn = db.conn();
        let hex = crate::sync::node_id_hex(peer);
        if let Err(e) = crate::db::set_setting(&conn, keys::SYNC_CACHED_PEER, &hex) {
            tracing::warn!(error = %e, "failed to cache sync peer");
        }
    }
}

/// Resolve the [`iroh::RelayMode`] for the transport: the hub's relay map when
/// signed in (persisting it as the offline cache), else the last cached map, else
/// iroh's default relays. Never fails — a resolution problem degrades to default
/// relays, so the transport always builds.
async fn resolve_relay_mode(ctx: &ServiceContext) -> iroh::RelayMode {
    let creds = crate::api::account::hub_credentials(ctx).unwrap_or(None);
    let cached = cached_relays(ctx).unwrap_or_default();
    let account = creds.as_ref().map(|(u, t)| (u.as_str(), t.as_str()));
    let res = pairing::resolve_relays(account, &cached).await;
    if res.fresh {
        store_cached_relays(ctx, &res.urls);
    }
    pairing::relay_mode_from_urls(&res.urls)
}

/// Resolve this device's sync peer following the documented order (task M1):
/// account pairing (capture role + paired primary, resolved from the hub, with
/// the last cached peer as an offline fallback) → dev-flag ticket → disabled.
/// The single seam shared by the app's capture-role sender and Perseus. A fresh
/// account resolution is cached for the next offline start.
pub async fn resolve_capture_peer(ctx: &ServiceContext) -> Result<PeerResolution, ApiError> {
    let account = crate::api::account::account_pairing(ctx)?;
    // The app has no *peer* ticket to dial: the dev flag only makes the app's own
    // receiver mint a ticket for Perseus to dial, so account pairing is the app's
    // sole capture-send route. Perseus keeps the ticket path (it holds one).
    let cached = cached_peer(ctx)?;
    let resolution = pairing::resolve_peer(account.as_ref(), None, cached).await;
    if let PeerResolution::Account { peer, fresh: true } = &resolution {
        store_cached_peer(ctx, peer);
    }
    Ok(resolution)
}

/// Boot-time autostart: if the dev flag is enabled, start the receiver +
/// transport now and return `true`; otherwise a no-op returning `false`. Called
/// at app start where the DB is already initialised (the web backend). On
/// desktop the DB is lazy, so the receiver instead starts on the first
/// [`get_pairing_ticket`] call.
pub async fn autostart_if_enabled(
    ctx: &ServiceContext,
    sync: &SyncRuntime,
    emitter: Arc<dyn ProgressEmitter>,
) -> Result<bool, ApiError> {
    if !dev_pairing_enabled(ctx)? {
        return Ok(false);
    }
    let (sync_dir, db_path) = sync_paths(ctx)?;
    let relay_mode = resolve_relay_mode(ctx).await;
    sync.ensure_started(sync_dir, db_path, relay_mode, emitter)
        .await
        .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    Ok(true)
}

/// Dev-flagged: lazily start the transport + receiver and return this device's
/// pairing ticket. `Forbidden` when the dev flag is off. The sync data (device
/// key, blob store, landed files) lives beside the catalog DB, under a `sync/`
/// sibling directory.
pub async fn get_pairing_ticket(
    ctx: &ServiceContext,
    sync: &SyncRuntime,
    emitter: Arc<dyn ProgressEmitter>,
) -> Result<String, ApiError> {
    // Dev-gate first; resolve paths and drop the DB borrow before awaiting.
    if !dev_pairing_enabled(ctx)? {
        return Err(ApiError::Forbidden(
            "personal sync is dev-gated; enable sync.dev_ticket_pairing first".into(),
        ));
    }
    let (sync_dir, db_path) = sync_paths(ctx)?;
    let relay_mode = resolve_relay_mode(ctx).await;

    let ticket = sync
        .ensure_started(sync_dir, db_path, relay_mode, emitter)
        .await
        .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    Ok(ticket)
}

/// Snapshot of the receive side for the Transfers UI.
pub async fn get_status(ctx: &ServiceContext, sync: &SyncRuntime) -> Result<SyncStatus, ApiError> {
    let dev_pairing_enabled = dev_pairing_enabled(ctx)?;
    let received_total = {
        let db = db(ctx)?;
        let conn = db.conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_history WHERE direction = 'received'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        n.max(0) as u32
    };
    let transport_started = sync.is_started().await;
    let pairing_ticket = sync.ticket().await;
    Ok(SyncStatus {
        dev_pairing_enabled,
        transport_started,
        pairing_ticket,
        received_total,
    })
}

/// The transfer history (received + sent), newest first.
pub fn list_history(ctx: &ServiceContext, query: SyncHistoryQuery) -> Result<Vec<HistoryRow>, ApiError> {
    let limit = if query.limit == 0 { DEFAULT_HISTORY_LIMIT } else { query.limit };
    let q = HistoryQuery {
        filename: query.filename,
        object: query.object,
        limit,
    };
    let db = db(ctx)?;
    let conn = db.conn();
    search_history_rows(&conn, &q).map_err(|e| ApiError::Internal(format!("{e:#}")))
}
