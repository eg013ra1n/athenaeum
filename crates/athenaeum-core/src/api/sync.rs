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

/// Clear the cached peer (task M1 review finding #2): the hub authoritatively
/// said the paired primary is gone or demoted, so a stale cached peer must not
/// keep being served on a later hub outage. Best-effort — a failure here just
/// means a possible one-time stale resolution on the next offline start.
fn clear_cached_peer(ctx: &ServiceContext) {
    if let Ok(db) = db(ctx) {
        let conn = db.conn();
        if let Err(e) = crate::db::delete_setting(&conn, keys::SYNC_CACHED_PEER) {
            tracing::warn!(error = %e, "failed to clear invalidated cached peer");
        }
    }
}

/// Resolve the [`iroh::RelayMode`] for the transport: the hub's relay map when
/// signed in (persisting it as the offline cache), else the last cached map.
/// Falling back to iroh's default relays beyond that requires the dev flag
/// (task M1 review finding #1) — otherwise this returns an actionable error
/// rather than silently starting the transport on public infrastructure.
async fn resolve_relay_mode(ctx: &ServiceContext) -> Result<iroh::RelayMode, ApiError> {
    let creds = crate::api::account::hub_credentials(ctx).unwrap_or(None);
    let cached = cached_relays(ctx).unwrap_or_default();
    let account = creds.as_ref().map(|(u, t)| (u.as_str(), t.as_str()));
    let res = pairing::resolve_relays(account, &cached).await;
    if res.fresh {
        store_cached_relays(ctx, &res.urls);
    }
    let allow_default = dev_pairing_enabled(ctx)?;
    pairing::relay_mode_for(&res.urls, allow_default).map_err(ApiError::Internal)
}

/// Resolve this device's sync peer following the documented order (task M1):
/// account pairing (capture role + paired primary, resolved from the hub, with
/// the last cached peer as an offline fallback) → dev-flag ticket → disabled.
/// The single seam shared by the app's capture-role sender and Perseus. A fresh
/// account resolution is cached for the next offline start; a hub-confirmed
/// gone/demoted peer ([`PeerResolution::Invalidated`]) clears that cache instead
/// (review finding #2) so a later hub outage can't resurrect a dead pairing.
pub async fn resolve_capture_peer(ctx: &ServiceContext) -> Result<PeerResolution, ApiError> {
    let account = crate::api::account::account_pairing(ctx)?;
    // The app has no *peer* ticket to dial: the dev flag only makes the app's own
    // receiver mint a ticket for Perseus to dial, so account pairing is the app's
    // sole capture-send route. Perseus keeps the ticket path (it holds one).
    let cached = cached_peer(ctx)?;
    let resolution = pairing::resolve_peer(account.as_ref(), None, cached).await;
    persist_peer_resolution(ctx, &resolution);
    Ok(resolution)
}

/// The cache side effects of a peer resolution (task M1 review finding #2): a
/// fresh account resolution refreshes the cache; a hub-confirmed
/// gone/demoted peer clears it instead, so a later hub outage can't resurrect a
/// pairing the hub already invalidated. Extracted from [`resolve_capture_peer`]
/// so it is unit-testable without exercising the account/keychain plumbing.
fn persist_peer_resolution(ctx: &ServiceContext, resolution: &PeerResolution) {
    match resolution {
        PeerResolution::Account { peer, fresh: true } => store_cached_peer(ctx, peer),
        PeerResolution::Invalidated { .. } => clear_cached_peer(ctx),
        _ => {}
    }
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
    let relay_mode = resolve_relay_mode(ctx).await?;
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
    let relay_mode = resolve_relay_mode(ctx).await?;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal real-`Database` [`ServiceContext`] (tempdir SQLite, no keychain
    /// involved anywhere) for exercising the settings-backed sync caches
    /// directly. Mirrors the construction pattern in `api::masters` tests.
    fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
        use crate::cache::MemoryImageCache;
        use crate::services::compute_queue::ComputeQueue;
        use crate::services::operation_queue::OperationQueue;
        use crate::settings::SettingsManager;
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        #[cfg(all(feature = "render", feature = "solver"))]
        use std::sync::RwLock;

        let tmp = tempfile::tempdir().unwrap();
        let database = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
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
        (tmp, ctx)
    }

    /// The peer cache round-trips: store → read back → clear → gone. Proves the
    /// exact mechanic [`persist_peer_resolution`] relies on for the
    /// `Invalidated` case (review finding #2's "assert the setting is gone").
    #[test]
    fn cached_peer_store_then_clear_removes_the_setting() {
        let (_tmp, ctx) = test_ctx();
        let peer = [7u8; 32];
        assert_eq!(cached_peer(&ctx).unwrap(), None, "nothing cached yet");

        store_cached_peer(&ctx, &peer);
        assert_eq!(cached_peer(&ctx).unwrap(), Some(peer));

        clear_cached_peer(&ctx);
        assert_eq!(cached_peer(&ctx).unwrap(), None, "the cached peer setting must be gone");
    }

    /// Review finding #2: `PeerResolution::Invalidated` clears an existing
    /// cached peer (not "leave it, next resolve wins") — a subsequent hub
    /// outage must not resurrect a pairing the hub already said is dead.
    #[test]
    fn invalidated_resolution_clears_an_existing_cache() {
        let (_tmp, ctx) = test_ctx();
        store_cached_peer(&ctx, &[1u8; 32]);
        assert!(cached_peer(&ctx).unwrap().is_some(), "precondition: a cache exists");

        persist_peer_resolution(
            &ctx,
            &PeerResolution::Invalidated { reason: "gone".to_string() },
        );
        assert_eq!(cached_peer(&ctx).unwrap(), None, "Invalidated must clear the cache");
    }

    /// A fresh account resolution stores the new peer as the cache.
    #[test]
    fn fresh_account_resolution_stores_the_cache() {
        let (_tmp, ctx) = test_ctx();
        let peer = [2u8; 32];
        persist_peer_resolution(&ctx, &PeerResolution::Account { peer, fresh: true });
        assert_eq!(cached_peer(&ctx).unwrap(), Some(peer));
    }

    /// A non-fresh (cached-fallback) resolution must NOT rewrite the cache —
    /// it already came FROM the cache, re-storing it is a harmless no-op at
    /// best and a footgun if the semantics ever drift.
    #[test]
    fn stale_account_resolution_does_not_touch_the_cache() {
        let (_tmp, ctx) = test_ctx();
        let original = [3u8; 32];
        store_cached_peer(&ctx, &original);

        persist_peer_resolution(
            &ctx,
            &PeerResolution::Account { peer: [9u8; 32], fresh: false },
        );
        assert_eq!(
            cached_peer(&ctx).unwrap(),
            Some(original),
            "a non-fresh resolution must not overwrite the existing cache"
        );
    }

    /// The relay-map cache round-trips through settings the same way.
    #[test]
    fn cached_relays_round_trip() {
        let (_tmp, ctx) = test_ctx();
        assert_eq!(cached_relays(&ctx).unwrap(), Vec::<String>::new());

        let urls = vec!["https://relay1.example.org".to_string(), "https://relay2.example.org".to_string()];
        store_cached_relays(&ctx, &urls);
        assert_eq!(cached_relays(&ctx).unwrap(), urls);
    }
}
