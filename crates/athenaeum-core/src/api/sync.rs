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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::api::{db, ApiError};
use crate::events::ProgressEmitter;
use crate::package::{self, ManifestRecord, PayloadKind, MANIFEST_VERSION};
use crate::services::ServiceContext;
use crate::settings::{defaults, keys};
use crate::sharing::types::NodeId;
use crate::sharing::SharingTransport;
use crate::sync::store::search_history_rows;
use crate::sync::{
    node_id_hex, pairing, CatalogSyncStore, HistoryQuery, HistoryRow, PeerResolution, StartedSender,
    SyncEngine, SyncEngineHandle, SyncRuntime, SyncSenderRuntime, SyncStatus, SyncStore,
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

/// One frame that could NOT be sent, with a user-facing reason (task M2). The
/// send path reports these back instead of silently dropping them.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct IneligibleFrame {
    pub frame_id: i64,
    pub reason: String,
}

/// Result of an [`enqueue_sync_selection`] / auto-mode enqueue: what was sent,
/// the `(N of M)` counts for the owner's mixed-selection convention, and the
/// ineligible remainder.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct EnqueueSelectionResult {
    /// Frames actually enqueued for send (equals `eligible_count`).
    pub enqueued_count: u32,
    /// Eligible frames — present on disk and resolvable in the catalog. The `N`.
    pub eligible_count: u32,
    /// Total frames requested. The `M` in `(N of M)`.
    pub total_count: u32,
    /// Frames that could not be sent, each with a reason. Never silently dropped.
    pub ineligible: Vec<IneligibleFrame>,
}

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

// ── App sender engine + manual/auto send (task M2) ───────────────────────────
//
// A capture-role app enqueues its own frames to the paired primary through a
// running sender-side [`SyncEngine`], the counterpart of the receiver's
// [`SyncRuntime`]. The engine is built lazily on the first enqueue and cached in
// the host `AppState`'s [`SyncSenderRuntime`]; the orchestration lives here (not
// in `sync::sender`) because it owns the account/pairing + iroh plumbing.

/// Map a resolved [`PeerResolution`] to a concrete peer id, or a typed error the
/// UI surfaces. A `Disabled` / `Invalidated` pairing returns an error HERE —
/// before any transport is built — so the send path never starts an engine on a
/// pairing the hub says is gone (task M2 self-review invariant).
fn peer_from_resolution(res: PeerResolution) -> Result<NodeId, ApiError> {
    match res {
        PeerResolution::Account { peer, .. } | PeerResolution::Ticket { peer } => Ok(peer),
        PeerResolution::Invalidated { reason } => Err(ApiError::Invalid(format!(
            "the paired primary was invalidated by the hub (re-pair in Settings): {reason}"
        ))),
        PeerResolution::Disabled { reason } => {
            Err(ApiError::Invalid(format!("personal sync is not configured: {reason}")))
        }
    }
}

/// The directory the app writes outgoing packages into (`<sync_dir>/packages`).
fn sender_packages_dir(ctx: &ServiceContext) -> Result<PathBuf, ApiError> {
    let (sync_dir, _db_path) = sync_paths(ctx)?;
    Ok(sync_dir.join("packages"))
}

/// Ensure the sender engine is running and return its handle + this device's
/// origin id. Idempotent: a started runtime short-circuits without resolving the
/// peer or building a transport. The very first call resolves the peer (guarded
/// — see [`peer_from_resolution`]), resolves the relay map, binds the shared
/// device identity's iroh transport, opens the catalog-backed store, and spawns
/// the engine.
///
/// The runtime mutex is held across the whole build so two concurrent enqueues
/// can never spawn two engines (the second blocks, then sees the populated slot).
pub async fn ensure_sender_engine(
    ctx: &ServiceContext,
    sender: &SyncSenderRuntime,
) -> Result<(Arc<SyncEngineHandle>, String), ApiError> {
    let mut guard = sender.lock_inner().await;
    if let Some(started) = guard.as_ref() {
        return Ok((Arc::clone(&started.engine), started.origin_device.clone()));
    }

    // Resolve the peer FIRST: a Disabled/Invalidated pairing errors out with NO
    // engine started, no transport bound, no package built.
    let resolution = resolve_capture_peer(ctx).await?;
    let peer = peer_from_resolution(resolution)?;
    let relay_mode = resolve_relay_mode(ctx).await?;
    let (sync_dir, db_path) = sync_paths(ctx)?;

    std::fs::create_dir_all(&sync_dir)
        .map_err(|e| ApiError::Internal(format!("create sync dir {}: {e}", sync_dir.display())))?;

    // The ONE device identity — the same key file the account layer + receiver
    // bind. Never a second identity.
    let secret = crate::account::keys::DeviceKey::load_or_create(
        &crate::account::keys::device_key_path(&sync_dir),
    )
    .map_err(|e| ApiError::Internal(format!("device key: {e:#}")))?
    .secret_bytes();

    let transport = crate::sharing::iroh::IrohTransport::new(
        secret,
        relay_mode,
        crate::sharing::iroh::BlobStore::Fs(sync_dir.join("blobs")),
    )
    .await
    .map_err(|e| ApiError::Internal(format!("build iroh transport for sender: {e:#}")))?;
    let origin_device = node_id_hex(&transport.node_id());
    let transport: Arc<dyn SharingTransport> = Arc::new(transport);

    let store = Arc::new(
        CatalogSyncStore::open(&db_path)
            .map_err(|e| ApiError::Internal(format!("open catalog sync store: {e:#}")))?,
    );
    let engine = Arc::new(SyncEngine::spawn(store as Arc<dyn SyncStore>, transport, peer));

    tracing::info!(peer = %node_id_hex(&peer), origin = %origin_device, "sync sender engine started");
    *guard = Some(StartedSender {
        engine: Arc::clone(&engine),
        origin_device: origin_device.clone(),
        peer,
    });
    Ok((engine, origin_device))
}

/// A built (or empty) selection package plus the eligibility split.
struct BuiltSelection {
    /// The written package directory, or `None` when nothing was eligible.
    pkg_dir: Option<PathBuf>,
    eligible: Vec<i64>,
    ineligible: Vec<IneligibleFrame>,
    total: usize,
}

/// A collision-free package `rel_path` for a payload. Uses the source filename;
/// on a duplicate basename within one package it disambiguates with the frame id
/// (and, in the pathological case, a uuid) so no two payloads overwrite.
fn unique_rel_path(filename: &str, frame_id: i64, used: &mut HashSet<String>) -> String {
    let base = if filename.trim().is_empty() {
        format!("frame_{frame_id}.fits")
    } else {
        filename.to_string()
    };
    if used.insert(base.clone()) {
        return base;
    }
    let mut candidate = format!("{frame_id}_{base}");
    while !used.insert(candidate.clone()) {
        candidate = format!("{}_{base}", uuid::Uuid::new_v4());
    }
    candidate
}

/// Build ONE package from exactly the eligible frames in `frame_ids`. INELIGIBLE
/// frames — not in the catalog, or whose file is missing/unreadable on disk — are
/// collected and returned, never silently dropped (task M2). The manifest mirrors
/// what Perseus builds (serialized `models::Frame` as `frame_meta` + the analysis
/// summary when present) so the primary ingests app- and Perseus-sourced frames
/// identically.
fn build_selection_package(
    conn: &rusqlite::Connection,
    origin_device: &str,
    packages_dir: &Path,
    frame_ids: &[i64],
) -> Result<BuiltSelection, ApiError> {
    // Dedup requested ids, preserving first-seen order for stable reporting.
    let mut seen_req = HashSet::new();
    let requested: Vec<i64> = frame_ids
        .iter()
        .copied()
        .filter(|id| seen_req.insert(*id))
        .collect();
    let total = requested.len();

    let rows = crate::db::get_frames_with_files_by_ids(conn, &requested)
        .map_err(|e| ApiError::Internal(format!("resolve frames for selection: {e:#}")))?;
    let analyses = crate::db::analysis::get_frame_analyses_by_ids(conn, &requested)
        .map_err(|e| ApiError::Internal(format!("load analysis summaries: {e:#}")))?;
    let analysis_by_frame: HashMap<i64, &crate::models::FrameAnalysis> =
        analyses.iter().map(|a| (a.frame_id, a)).collect();

    let mut resolved: HashSet<i64> = HashSet::new();
    let mut ineligible: Vec<IneligibleFrame> = Vec::new();
    let mut eligible: Vec<i64> = Vec::new();
    let mut records: Vec<(PathBuf, ManifestRecord)> = Vec::new();
    let mut used_rel_paths: HashSet<String> = HashSet::new();

    for (_file_id, file, frame) in &rows {
        let Some(frame_id) = frame.id else { continue };
        resolved.insert(frame_id);

        let path = Path::new(&file.path);
        if !path.exists() {
            ineligible.push(IneligibleFrame { frame_id, reason: "file missing on disk".to_string() });
            continue;
        }
        let byte_size = match std::fs::metadata(path) {
            Ok(m) => m.len(),
            Err(e) => {
                ineligible.push(IneligibleFrame { frame_id, reason: format!("cannot stat file: {e}") });
                continue;
            }
        };
        let xxh3 = match package::xxh3_full_file(path) {
            Ok(h) => h,
            Err(e) => {
                ineligible.push(IneligibleFrame { frame_id, reason: format!("cannot read file: {e:#}") });
                continue;
            }
        };
        let frame_meta = match serde_json::to_value(frame) {
            Ok(v) => v,
            Err(e) => {
                ineligible.push(IneligibleFrame { frame_id, reason: format!("serialize frame_meta: {e}") });
                continue;
            }
        };
        let analysis = match analysis_by_frame.get(&frame_id) {
            Some(a) => match serde_json::to_value(a) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(frame_id, error = %e, "sync selection: analysis serialize failed; omitting");
                    None
                }
            },
            None => None,
        };

        // Identity anchor: the catalog frame uuid (the receiver dedups on it).
        let frame_uuid = frame
            .uuid
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let rel_path = unique_rel_path(&file.filename, frame_id, &mut used_rel_paths);

        records.push((
            path.to_path_buf(),
            ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: frame_uuid.clone(),
                origin_catalog_uuid: frame_uuid,
                origin_device: origin_device.to_string(),
                payload_kind: PayloadKind::RawFrame,
                rel_path,
                byte_size,
                xxh3,
                frame_meta,
                analysis,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            },
        ));
        eligible.push(frame_id);
    }

    // Requested ids that never resolved to a catalog row at all.
    for id in &requested {
        if !resolved.contains(id) {
            ineligible.push(IneligibleFrame {
                frame_id: *id,
                reason: "frame not found in catalog".to_string(),
            });
        }
    }

    if records.is_empty() {
        return Ok(BuiltSelection { pkg_dir: None, eligible, ineligible, total });
    }

    let pkg_dir = packages_dir.join(uuid::Uuid::new_v4().to_string());
    package::write_package(&pkg_dir, records)
        .map_err(|e| ApiError::Internal(format!("write selection package: {e:#}")))?;
    Ok(BuiltSelection { pkg_dir: Some(pkg_dir), eligible, ineligible, total })
}

/// Build the selection package and enqueue it into `engine`. The transport-
/// agnostic core shared by the manual command and the auto-mode hook — exercised
/// in tests against a loopback-backed engine. The DB borrow is dropped before the
/// (async) enqueue so no connection guard is ever held across an `.await`.
async fn build_and_enqueue_selection(
    ctx: &ServiceContext,
    engine: &SyncEngineHandle,
    origin_device: &str,
    packages_dir: &Path,
    frame_ids: &[i64],
) -> Result<EnqueueSelectionResult, ApiError> {
    let built = {
        let db = db(ctx)?;
        let conn = db.conn();
        build_selection_package(&conn, origin_device, packages_dir, frame_ids)?
    };
    if let Some(dir) = &built.pkg_dir {
        engine
            .enqueue_package(dir)
            .await
            .map_err(|e| ApiError::Internal(format!("enqueue selection package: {e:#}")))?;
    }
    Ok(EnqueueSelectionResult {
        enqueued_count: built.eligible.len() as u32,
        eligible_count: built.eligible.len() as u32,
        total_count: built.total as u32,
        ineligible: built.ineligible,
    })
}

/// Manual send (task M2, step 2): enqueue exactly the eligible frames in the
/// selection to the paired primary as ONE package. Ineligible frames come back in
/// the result. Starting the engine is the send path's hard pairing guard — a
/// Disabled/Invalidated peer errors here before anything is built.
///
/// An empty selection is a benign no-op checked FIRST (task M2 review minor):
/// there is nothing to resolve or send, so it must never surface a pairing
/// error on an otherwise-unconfigured device just because the caller happened
/// to pass zero ids.
pub async fn enqueue_sync_selection(
    ctx: &ServiceContext,
    sender: &SyncSenderRuntime,
    frame_ids: Vec<i64>,
) -> Result<EnqueueSelectionResult, ApiError> {
    if frame_ids.is_empty() {
        return Ok(EnqueueSelectionResult {
            enqueued_count: 0,
            eligible_count: 0,
            total_count: 0,
            ineligible: Vec::new(),
        });
    }
    let (engine, origin_device) = ensure_sender_engine(ctx, sender).await?;
    let packages_dir = sender_packages_dir(ctx)?;
    let result =
        build_and_enqueue_selection(ctx, &engine, &origin_device, &packages_dir, &frame_ids).await?;
    tracing::info!(
        enqueued = result.enqueued_count,
        total = result.total_count,
        ineligible = result.ineligible.len(),
        "sync selection enqueued"
    );
    Ok(result)
}

/// Whether full-app capture-node auto mode is enabled (`sync.auto_mode`).
pub fn get_sync_auto_mode(ctx: &ServiceContext) -> Result<bool, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let v = ctx
        .settings
        .get_with_precedence(&conn, keys::SYNC_AUTO_MODE, defaults::SYNC_AUTO_MODE)?;
    Ok(v.eq_ignore_ascii_case("true"))
}

/// Toggle full-app capture-node auto mode.
pub fn set_sync_auto_mode(ctx: &ServiceContext, enabled: bool) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    crate::db::set_setting(&conn, keys::SYNC_AUTO_MODE, if enabled { "true" } else { "false" })?;
    tracing::info!(enabled, "sync auto mode set");
    Ok(())
}

/// The auto-mode entry guard: auto mode on AND this device is a signed-in
/// `capture` node with a paired primary. Read purely from settings — a signed-out
/// device has its role + peer cleared (`clear_local_session`), so this is a
/// reliable, network-free proxy for "capture + signed in + paired". The
/// authoritative signed-in check still runs downstream at peer resolution in
/// [`ensure_sender_engine`]; this only decides whether to attempt at all, so a
/// primary or signed-out device never even builds a package.
fn auto_mode_ready(ctx: &ServiceContext) -> Result<bool, ApiError> {
    if !get_sync_auto_mode(ctx)? {
        return Ok(false);
    }
    let db = db(ctx)?;
    let conn = db.conn();
    let role = crate::db::get_setting(&conn, keys::ACCOUNT_ROLE)?
        .and_then(|s| crate::account::DeviceRole::parse(&s));
    let has_peer = crate::db::get_setting(&conn, keys::ACCOUNT_PEER_DEVICE_ID)?
        .filter(|s| !s.is_empty())
        .is_some();
    Ok(role == Some(crate::account::DeviceRole::Capture) && has_peer)
}

/// Auto mode (task M2, step 3): the scanner "scan finished" hook. When auto mode
/// is on for a signed-in capture node, the files newly ingested by that scan are
/// enqueued to the primary as ONE per-scan-batch package (the same builder the
/// manual command uses). A per-batch package keeps one scan → one confirm/retry
/// unit; per-file would multiply the announce/ack traffic for no gain.
///
/// Guards live INSIDE the hook, not just the UI: a primary, signed-out, or
/// auto-off device returns `Ok(None)` and enqueues nothing. A sync failure
/// (unreachable peer, etc.) is logged and swallowed — it must never fail the scan
/// that triggered it.
pub async fn auto_enqueue_scanned_files(
    ctx: &ServiceContext,
    sender: &SyncSenderRuntime,
    file_ids: Vec<i64>,
) -> Result<Option<EnqueueSelectionResult>, ApiError> {
    if !auto_mode_ready(ctx)? {
        return Ok(None);
    }
    if file_ids.is_empty() {
        return Ok(None);
    }
    // The scanner reports FILE ids; the package is keyed on FRAME ids.
    let frame_ids = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::get_frame_ids_for_file_ids(&conn, &file_ids)
            .map_err(|e| ApiError::Internal(format!("resolve frame ids for scanned files: {e:#}")))?
    };
    if frame_ids.is_empty() {
        return Ok(None);
    }
    match enqueue_sync_selection(ctx, sender, frame_ids).await {
        Ok(result) => {
            tracing::info!(enqueued = result.enqueued_count, "auto-mode sync enqueued scanned files");
            Ok(Some(result))
        }
        Err(e) => {
            tracing::warn!(error = %e, "auto-mode sync enqueue failed; scan unaffected");
            Ok(None)
        }
    }
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

    // ── Manual/auto send (task M2) ───────────────────────────────────────────

    /// Write a fixture file on disk + insert its `files`/`frames` rows; returns
    /// the frame id. With `analyze`, also inserts a `frame_analysis` summary.
    fn insert_fixture_frame(
        ctx: &ServiceContext,
        dir: &std::path::Path,
        filename: &str,
        object: &str,
        analyze: bool,
    ) -> i64 {
        use crate::models::{File, FileFormat, Frame, FrameAnalysis};
        let path = dir.join(filename);
        std::fs::write(&path, format!("payload-{filename}").as_bytes()).unwrap();
        let size = std::fs::metadata(&path).unwrap().len() as i64;

        let db = db(ctx).unwrap();
        let conn = db.conn();
        let file = File {
            id: None,
            path: path.to_string_lossy().to_string(),
            filename: filename.to_string(),
            size,
            modified_at: chrono::Utc::now(),
            format: FileFormat::FITS,
            created_at: chrono::Utc::now(),
            metadata_hash: None,
            content_hash: None,
            archived_in_operation: None,
            archive_zip_path: None,
            archive_path_in_zip: None,
            uuid: None,
            updated_at: None,
        };
        let file_id = crate::db::insert_file(&conn, &file).unwrap();
        let frame = Frame { file_id, object: Some(object.to_string()), ..Default::default() };
        let frame_id = crate::db::insert_frame(&conn, &frame).unwrap();
        if analyze {
            let a = FrameAnalysis {
                id: None,
                frame_id,
                file_id,
                stars_detected: 123,
                median_fwhm: 2.5,
                median_eccentricity: 0.4,
                median_snr: 30.0,
                median_hfr: 1.8,
                frame_snr: 40.0,
                snr_weight: 1.0,
                psf_signal: 100.0,
                background: 10.0,
                noise: 2.0,
                detection_threshold: 5.0,
                width: 1000,
                height: 800,
                source_channels: 1,
                trail_r_squared: 0.0,
                possibly_trailed: false,
                median_beta: Some(3.0),
                quality_score: Some(0.9),
                config_hash: Some("cfg".to_string()),
                analyzed_at: "2026-07-06T00:00:00Z".to_string(),
            };
            crate::db::analysis::upsert_frame_analysis(&conn, &a).unwrap();
        }
        frame_id
    }

    /// Inject a loopback-backed engine into `sender` as though
    /// [`ensure_sender_engine`] had already built it — no hub, no iroh. Writes
    /// into `ctx`'s own catalog sync tables so the test can inspect the outbound
    /// rows it produces.
    async fn inject_loopback_sender(ctx: &ServiceContext, sender: &SyncSenderRuntime) {
        use crate::sharing::loopback::LoopbackNetwork;
        let db_path = db(ctx).unwrap().path().to_path_buf();
        let store = Arc::new(CatalogSyncStore::open(&db_path).unwrap());
        let net = LoopbackNetwork::new();
        let transport: Arc<dyn SharingTransport> = Arc::new(net.endpoint());
        let peer: NodeId = [9u8; 32];
        let engine = Arc::new(SyncEngine::spawn(store as Arc<dyn SyncStore>, transport, peer));
        let mut guard = sender.lock_inner().await;
        *guard = Some(StartedSender { engine, origin_device: "aa".repeat(32), peer });
    }

    fn outbound_package_refs(ctx: &ServiceContext) -> Vec<String> {
        let db = db(ctx).unwrap();
        let conn = db.conn();
        let mut stmt = conn.prepare("SELECT package_ref FROM sync_outbound").unwrap();
        let refs = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();
        refs
    }

    /// Step 1: a selection builds ONE package from exactly the eligible frames,
    /// carrying each frame's serialized `Frame` as `frame_meta` and its analysis
    /// summary when present — never the unselected frame.
    #[test]
    fn enqueue_selection_builds_exact_package() {
        let (tmp, ctx) = test_ctx();
        let dir = tmp.path();
        let f1 = insert_fixture_frame(&ctx, dir, "light-0001.fits", "M42", true);
        let f2 = insert_fixture_frame(&ctx, dir, "light-0002.fits", "M42", false);
        let _f3 = insert_fixture_frame(&ctx, dir, "light-0003.fits", "M42", false);

        let pkg_root = tmp.path().join("packages");
        let built = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            build_selection_package(&conn, "origin-dev", &pkg_root, &[f1, f2]).unwrap()
        };
        assert_eq!(built.eligible.len(), 2);
        assert!(built.ineligible.is_empty());
        assert_eq!(built.total, 2);

        let dir = built.pkg_dir.expect("a package was written");
        let records = crate::package::read_manifest(&dir).unwrap();
        assert_eq!(records.len(), 2, "exactly the selection, not the unselected f3");
        for r in &records {
            let f: crate::models::Frame = serde_json::from_value(r.frame_meta.clone()).unwrap();
            assert_eq!(f.object.as_deref(), Some("M42"), "frame_meta is the catalog Frame");
        }
        let analyzed: Vec<&crate::package::ManifestRecord> =
            records.iter().filter(|r| r.analysis.is_some()).collect();
        assert_eq!(analyzed.len(), 1, "analysis summary included only when present");
        let af: crate::models::Frame =
            serde_json::from_value(analyzed[0].frame_meta.clone()).unwrap();
        assert_eq!(af.id, Some(f1), "the analysis is attached to the analyzed frame");
    }

    /// Step 1: ineligible files (missing on disk, or an unknown id) are reported
    /// back with reasons — never silently dropped — while the eligible remainder
    /// still enqueues.
    #[test]
    fn ineligible_files_reported_not_dropped() {
        let (tmp, ctx) = test_ctx();
        let dir = tmp.path();
        let f1 = insert_fixture_frame(&ctx, dir, "light-0001.fits", "M42", false);
        let f2 = insert_fixture_frame(&ctx, dir, "light-0002.fits", "M42", false);
        std::fs::remove_file(dir.join("light-0002.fits")).unwrap(); // f2 → missing on disk
        let unknown = 999_999; // never inserted → not found

        let pkg_root = tmp.path().join("packages");
        let built = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            build_selection_package(&conn, "origin-dev", &pkg_root, &[f1, f2, unknown]).unwrap()
        };
        assert_eq!(built.eligible, vec![f1], "only the present file is eligible");
        assert_eq!(built.total, 3);
        let reasons: HashMap<i64, String> =
            built.ineligible.iter().map(|i| (i.frame_id, i.reason.clone())).collect();
        assert!(reasons.get(&f2).unwrap().contains("missing"), "f2 reported missing");
        assert!(reasons.get(&unknown).unwrap().contains("not found"), "unknown id reported");

        let records = crate::package::read_manifest(&built.pkg_dir.unwrap()).unwrap();
        assert_eq!(records.len(), 1, "the eligible frame still enqueues");
    }

    /// Step 3: the auto-mode hook. With auto on for a signed-in capture node, N
    /// newly-scanned files become ONE batch package of N records; auto off →
    /// nothing; a non-capture role → nothing (and no engine is started in either
    /// negative case).
    #[tokio::test]
    async fn auto_mode_scan_enqueues_new_files() {
        let (tmp, ctx) = test_ctx();
        let dir = tmp.path();
        let f1 = insert_fixture_frame(&ctx, dir, "light-0001.fits", "M42", false);
        let f2 = insert_fixture_frame(&ctx, dir, "light-0002.fits", "M42", false);
        let file_ids: Vec<i64> = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let mut stmt = conn.prepare("SELECT file_id FROM frames WHERE id IN (?1, ?2)").unwrap();
            stmt.query_map([f1, f2], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<i64>>>()
                .unwrap()
        };

        let sender = SyncSenderRuntime::new();

        // auto OFF → nothing enqueued, no engine built.
        set_sync_auto_mode(&ctx, false).unwrap();
        let none = auto_enqueue_scanned_files(&ctx, &sender, file_ids.clone()).await.unwrap();
        assert!(none.is_none(), "auto off → nothing");
        assert!(!sender.is_started().await, "no engine when auto off");

        // auto ON but role != capture (unset) → nothing, still no engine.
        set_sync_auto_mode(&ctx, true).unwrap();
        let none = auto_enqueue_scanned_files(&ctx, &sender, file_ids.clone()).await.unwrap();
        assert!(none.is_none(), "non-capture role → nothing");
        assert!(!sender.is_started().await, "no engine for a non-capture device");

        // auto ON + signed-in capture + paired → enqueue one batch package.
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            crate::db::set_setting(&conn, keys::ACCOUNT_ROLE, "capture").unwrap();
            crate::db::set_setting(&conn, keys::ACCOUNT_PEER_DEVICE_ID, "primary-1").unwrap();
        }
        inject_loopback_sender(&ctx, &sender).await; // stand in for ensure_sender_engine
        let result = auto_enqueue_scanned_files(&ctx, &sender, file_ids.clone())
            .await
            .unwrap()
            .expect("capture + auto on enqueues");
        assert_eq!(result.enqueued_count, 2, "both new files enqueued");
        assert_eq!(result.total_count, 2);

        let refs = outbound_package_refs(&ctx);
        assert_eq!(refs.len(), 1, "one per-scan-batch package (not per-file)");
        let records = crate::package::read_manifest(std::path::Path::new(&refs[0])).unwrap();
        assert_eq!(records.len(), 2, "N records in the single batch package");
    }

    /// Engine-start guard: the resolution → peer mapping turns a
    /// Disabled/Invalidated pairing into a typed error (so the send path never
    /// starts an engine), and an Account/Ticket into the concrete peer.
    #[test]
    fn peer_from_resolution_gates_disabled_and_invalidated() {
        assert!(matches!(
            peer_from_resolution(PeerResolution::Account { peer: [1u8; 32], fresh: true }),
            Ok(p) if p == [1u8; 32]
        ));
        assert!(matches!(
            peer_from_resolution(PeerResolution::Ticket { peer: [2u8; 32] }),
            Ok(p) if p == [2u8; 32]
        ));
        assert!(matches!(
            peer_from_resolution(PeerResolution::Disabled { reason: "x".into() }),
            Err(ApiError::Invalid(_))
        ));
        assert!(matches!(
            peer_from_resolution(PeerResolution::Invalidated { reason: "y".into() }),
            Err(ApiError::Invalid(_))
        ));
    }

    /// Engine-start guard, end to end: a signed-out device resolves to a
    /// Disabled pairing, so `enqueue_sync_selection` returns a typed error and
    /// NO engine is started. The bogus hub host makes the token load a clean
    /// keychain miss (no prompt) and no network call is ever made (account = None
    /// short-circuits the resolver before any hub request).
    #[tokio::test]
    async fn enqueue_on_unconfigured_pairing_errors_without_starting_engine() {
        let (_tmp, ctx) = test_ctx();
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            crate::db::set_setting(&conn, keys::ACCOUNT_HUB_URL, "http://m2-guard-unconfigured.invalid")
                .unwrap();
        }
        let sender = SyncSenderRuntime::new();
        let err = enqueue_sync_selection(&ctx, &sender, vec![1, 2, 3]).await.unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "unconfigured pairing → typed error, got {err:?}");
        assert!(!sender.is_started().await, "no engine started on a Disabled pairing");
    }

    /// Task M2 review minor: an empty selection is a benign no-op checked
    /// BEFORE `ensure_sender_engine` — it must return the zero result rather
    /// than surfacing a pairing error, and it must never even attempt to
    /// resolve the peer. Proven two ways: (1) the call succeeds with a
    /// zero-valued result and no engine is started, where the OLD ordering
    /// (empty check after `ensure_sender_engine`) would have returned
    /// `Err(ApiError::Invalid)` for this same signed-out device; (2) a
    /// wiremock hub records zero requests.
    #[tokio::test]
    async fn enqueue_with_empty_selection_returns_zero_result_without_starting_engine() {
        let (_tmp, ctx) = test_ctx();
        let server = wiremock::MockServer::start().await;
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            crate::db::set_setting(&conn, keys::ACCOUNT_HUB_URL, &server.uri()).unwrap();
        }
        let sender = SyncSenderRuntime::new();

        let result = enqueue_sync_selection(&ctx, &sender, Vec::new()).await.unwrap();
        assert_eq!(result.enqueued_count, 0);
        assert_eq!(result.eligible_count, 0);
        assert_eq!(result.total_count, 0);
        assert!(result.ineligible.is_empty());
        assert!(!sender.is_started().await, "no engine started for an empty selection");

        let requests = server.received_requests().await.unwrap();
        assert!(requests.is_empty(), "empty selection must never resolve the peer against the hub");
    }
}
