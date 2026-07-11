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
    node_id_hex, pairing, CatalogSyncStore, Direction, HistoryQuery, HistoryRow, OutboundRow,
    OutboundState, OutboundSummary, StartedSender, SyncEngine, SyncEngineHandle,
    SyncReceiverStatus, SyncRuntime, SyncSenderRuntime, SyncSenderStatus, SyncStatus, SyncStore,
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
    /// Restrict to one direction (`sent` / `received`); unfiltered when absent.
    pub direction: Option<Direction>,
    /// Exact peer node id (hex) filter; unfiltered when absent.
    pub peer: Option<String>,
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

/// Result of an [`enqueue_sync_selection`]: what was sent, the `(N of M)` counts
/// for the owner's mixed-selection convention, and the ineligible remainder.
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

/// Build the receiver's live per-package landing resolver. It re-reads the
/// designated `sync_incoming` scan root from the catalog on **every** package —
/// so designating or clearing that root (task 4) takes effect on the next
/// received package with no transport restart — falling back to `fallback`
/// (`<sync_dir>/incoming`) when no root is designated. A lookup error is logged,
/// never swallowed, and also falls back (never strands an inbound package).
///
/// The closure is `'static`: it captures a cheap cloned [`crate::db::Database`]
/// handle (shared pool) rather than borrowing `ctx`, so it can outlive this call
/// and run inside the receiver's spawned loop.
fn incoming_resolver(
    ctx: &ServiceContext,
    fallback: PathBuf,
) -> Result<crate::sync::receiver::IncomingResolver, ApiError> {
    let db = db(ctx)?.clone();
    Ok(Arc::new(move || {
        let conn = db.conn();
        match crate::db::scan_root_path_of_kind(&conn, "sync_incoming") {
            Ok(Some(p)) => PathBuf::from(p),
            Ok(None) => fallback.clone(),
            Err(e) => {
                tracing::warn!(error = %e, "sync_incoming root lookup failed; landing in app-data fallback");
                fallback.clone()
            }
        }
    }))
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

// ── authorized inbound peers (finding H1) ────────────────────────────────────

/// Decode a base64 device pubkey (`AccountDevice::pubkey`) into its 64-char
/// lowercase hex node id — the form the allow-list and the transport compare.
fn pubkey_b64_to_hex(pubkey_b64: &str) -> Option<String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(pubkey_b64)
        .ok()?;
    let arr: crate::sharing::types::NodeId = bytes.as_slice().try_into().ok()?;
    Some(crate::sync::node_id_hex(&arr))
}

/// The hex node ids of every device in the account — the receiver's allow-list
/// in the mesh model (finding H1, updated for sync Phase 1): any device in my
/// account is trusted. Order is preserved; undecodable pubkeys are skipped.
fn account_peer_hexes(devices: &[crate::account::AccountDevice]) -> Vec<String> {
    devices.iter().filter_map(|d| pubkey_b64_to_hex(&d.pubkey)).collect()
}

/// Build the receiver's live peer-authorization gate (finding H1). A signed-in
/// node enforces the cached allow-list (`SYNC_AUTHORIZED_PEERS`), re-read from
/// settings on **every** announce so a hub refresh takes effect on the next
/// package. A pure dev-ticket node (no account) has no hub to build a list from,
/// so it accepts any peer — the dev flag is a developer-only escape hatch.
///
/// Fail closed: a signed-in node whose cache is empty (never refreshed)
/// authorizes nobody until [`refresh_authorized_peers`] populates it.
fn peer_authorizer(ctx: &ServiceContext) -> Result<crate::sync::PeerAuthorizer, ApiError> {
    if !account_signed_in(ctx)? {
        tracing::warn!(
            "sync receiver has no account allow-list (dev-ticket mode); accepting any peer"
        );
        return Ok(crate::sync::allow_all_peers());
    }
    let db = db(ctx)?.clone();
    Ok(Arc::new(move |from: &crate::sharing::types::NodeId| {
        let hex = crate::sync::node_id_hex(from);
        let conn = db.conn();
        match crate::db::get_setting(&conn, keys::SYNC_AUTHORIZED_PEERS) {
            Ok(Some(raw)) => raw.lines().map(str::trim).any(|line| line == hex),
            _ => false, // fail closed: no list yet ⇒ authorize nobody
        }
    }))
}

/// Refresh the cached authorized-peer allow-list from the hub device list
/// (finding H1). Best-effort: on any credential/hub failure the existing cache
/// is left untouched (a node that synced before keeps working offline; a
/// brand-new one authorizes nobody until it can reach the hub — fail closed).
/// Every device in the account is admitted (mesh model, finding H1 as updated
/// for sync Phase 1).
pub async fn refresh_authorized_peers(ctx: &ServiceContext) {
    if !matches!(account_signed_in(ctx), Ok(true)) {
        return;
    }
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx).ok().flatten() else {
        return;
    };
    let client = match crate::account::HubClient::new(hub_url) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "authorized-peer refresh: hub client build failed");
            return;
        }
    };
    let devices = match client.list_devices(&token).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "authorized-peer refresh: device list unavailable; keeping cached set");
            return;
        }
    };
    let hexes = account_peer_hexes(&devices);
    if let Ok(db) = db(ctx) {
        let conn = db.conn();
        if let Err(e) = crate::db::set_setting(&conn, keys::SYNC_AUTHORIZED_PEERS, &hexes.join("\n"))
        {
            tracing::warn!(error = %e, "failed to cache authorized peers");
        } else {
            tracing::info!(count = hexes.len(), "refreshed authorized account peers");
        }
    }
}

/// Whether the dev-only default-relay fallback ([`pairing::relay_mode_for`]'s
/// `allow_default`) is permitted, given whether this device has hub credentials
/// (`signed_in`) and whether the dev pairing flag is on (`dev_flag`).
///
/// **Signed-in always wins, even with the dev flag on** (fix-review addendum):
/// a signed-in device must never end up on iroh's public n0 default relays —
/// observed in production, toggling the dev flag as a workaround put a
/// signed-in app node on n0 while its account-mode Perseus peer sat on the
/// hub's relay map, so dial-by-node-id failed instantly (different relay
/// networks). The dev-only Default fallback is for **pure ticket/dev mode**
/// only: no account at all. A transient "signed in but nothing resolved yet"
/// moment (hub blip + empty cache) must refuse loudly instead of silently
/// building a transport on the wrong relays — [`SyncRuntime::ensure_started`]
/// caches whatever it builds for the process lifetime, so a wrong first choice
/// would otherwise stick until restart.
fn allow_default_relays(signed_in: bool, dev_flag: bool) -> bool {
    !signed_in && dev_flag
}

/// Resolve the [`iroh::RelayMode`] for the transport — and the raw relay URLs
/// it was built from, needed by [`ensure_sender_engine`] to attach a dial hint
/// to an account-resolved peer (fix-review: a bare node id is undialable, see
/// [`pairing::peer_addr_with_relays`]). The hub's relay map when signed in
/// (persisting it as the offline cache), else the last cached map. Falling
/// back to iroh's default relays beyond that requires the dev flag AND being
/// signed out ([`allow_default_relays`]) — otherwise this returns an
/// actionable error rather than silently starting the transport on public
/// infrastructure (or, worse, mixed relay networks with a signed-in peer).
async fn resolve_relay_mode(ctx: &ServiceContext) -> Result<(iroh::RelayMode, Vec<String>), ApiError> {
    let creds = crate::api::account::hub_credentials(ctx).unwrap_or(None);
    let cached = cached_relays(ctx).unwrap_or_default();
    let account = creds.as_ref().map(|(u, t)| (u.as_str(), t.as_str()));
    let res = pairing::resolve_relays(account, &cached).await;
    if res.fresh {
        store_cached_relays(ctx, &res.urls);
    }
    let allow_default = allow_default_relays(creds.is_some(), dev_pairing_enabled(ctx)?);
    let mode = pairing::relay_mode_for(&res.urls, allow_default).map_err(ApiError::Internal)?;
    Ok((mode, res.urls))
}

/// Local-state-only "is this node signed in" check for [`autostart_if_enabled`]:
/// the persisted `ACCOUNT_DEVICE_ID` the app writes on sign-in / clears on
/// sign-out by `clear_local_session`. Every signed-in Athenaeum node is a full
/// peer (capability `athenaeum`) and runs a receiver — there is no role gate
/// (sync Phase 1 mesh model). Deliberately settings-only:
/// never touches the hub or the OS keychain, so the boot path can decide
/// "should the receiver even try to start" without a network round-trip or a
/// keychain call.
fn account_signed_in(ctx: &ServiceContext) -> Result<bool, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_setting(&conn, keys::ACCOUNT_DEVICE_ID)?
        .filter(|s| !s.is_empty())
        .is_some())
}

/// Boot-time autostart: start the receiver + transport when EITHER the dev
/// pairing flag is enabled OR this device is signed in
/// ([`account_signed_in`], task A7 fix-review + sync Phase 2A — every signed-in
/// mesh node must listen without anyone opening the dev-ticket disclosure, and
/// with no `role == Primary` gate).
/// Returns `true` iff it (re)confirmed the receiver is running, `false` when
/// neither condition holds. The condition check is local-state-only (no hub
/// call to decide *whether* to start); `resolve_relay_mode` may still reach the
/// hub for a fresh relay map once the decision is "yes", falling back to the
/// cached map (or refusing — never iroh's public defaults for a signed-in
/// device, see [`allow_default_relays`]) if it's unreachable.
///
/// Called at app start where the DB is already initialised (the web backend,
/// and — since this fix — the desktop host right after `initialize_database`
/// succeeds, as desktop's DB is populated lazily by the frontend rather than at
/// Tauri `setup()`). Idempotent regardless of call site:
/// [`SyncRuntime::ensure_started`] only ever builds the transport once.
pub async fn autostart_if_enabled(
    ctx: &ServiceContext,
    sync: &SyncRuntime,
    emitter: Arc<dyn ProgressEmitter>,
) -> Result<bool, ApiError> {
    let dev = dev_pairing_enabled(ctx)?;
    let signed_in = account_signed_in(ctx)?;
    if !autostart_gate(dev, signed_in) {
        return Ok(false);
    }
    tracing::debug!(dev, signed_in, "sync autostart condition met");
    let (sync_dir, db_path) = sync_paths(ctx)?;
    let incoming = incoming_resolver(ctx, sync_dir.join("incoming"))?;
    // The receiver only listens; it never dials a peer for announce, so the raw
    // relay URLs (needed only to construct a dial hint) are irrelevant here.
    let (relay_mode, _relay_urls) = resolve_relay_mode(ctx).await?;
    // Populate the authorized-peer allow-list (best-effort) before the receiver
    // starts accepting, then enforce it live per-package (finding H1).
    refresh_authorized_peers(ctx).await;
    let authorized = peer_authorizer(ctx)?;
    sync.ensure_started(sync_dir, db_path, relay_mode, incoming, authorized, emitter)
        .await
        .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    Ok(true)
}

/// The pure condition [`autostart_if_enabled`] gates on: the dev pairing flag,
/// OR this device being signed in (any Athenaeum node — no role gate). Factored
/// out for a fast, network-free unit test of the exact matrix (task A7
/// fix-review, broadened in sync Phase 2A).
fn autostart_gate(dev: bool, signed_in: bool) -> bool {
    dev || signed_in
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
    let incoming = incoming_resolver(ctx, sync_dir.join("incoming"))?;
    // Same reasoning as `autostart_if_enabled`: the receiver never dials out
    // for announce, so the raw relay URLs are not needed here.
    let (relay_mode, _relay_urls) = resolve_relay_mode(ctx).await?;
    // Enforce the authorized-peer allow-list here too (finding H1). In pure
    // dev-ticket mode (no account) this resolves to accept-any; a signed-in
    // primary that also flips the dev flag still enforces its account list.
    refresh_authorized_peers(ctx).await;
    let authorized = peer_authorizer(ctx)?;

    let ticket = sync
        .ensure_started(sync_dir, db_path, relay_mode, incoming, authorized, emitter)
        .await
        .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    Ok(ticket)
}

/// Shorten an identifier for display (peer hex / package uuid) — the leading 10
/// chars, enough to disambiguate at a glance without a wall of hex.
fn short_id(s: &str) -> String {
    let t = s.trim();
    if t.chars().count() <= 10 {
        t.to_string()
    } else {
        t.chars().take(10).collect()
    }
}

/// Short display handle for an outbound package (the basename of its dir).
fn short_pkg(package_ref: &str) -> String {
    let base = std::path::Path::new(package_ref)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(package_ref);
    short_id(base)
}

/// Count `sync_outbound` rows in a given terminal `state`.
fn count_outbound_state(conn: &rusqlite::Connection, state: &str) -> Result<u32, ApiError> {
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_outbound WHERE state = ?1", [state], |r| r.get(0))
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(n.max(0) as u32)
}

/// Total frames received (history rows with `direction = received`).
fn received_total(ctx: &ServiceContext) -> Result<u32, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_history WHERE direction = 'received'", [], |r| r.get(0))
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(n.max(0) as u32)
}

/// The send-side rollup: live in-flight counts + rows from the engine's
/// non-terminal snapshot, plus terminal totals counted from `sync_outbound`.
async fn build_sender_status(
    ctx: &ServiceContext,
    sender: &SyncSenderRuntime,
) -> Result<SyncSenderStatus, ApiError> {
    // Roll up the live snapshot of EVERY started peer engine (per-peer map,
    // sync 2C) under one lock so the in-flight view spans all destinations.
    let (started, active_rows) = {
        let guard = sender.lock_inner().await;
        let started = !guard.is_empty();
        let mut active_rows: Vec<OutboundRow> = Vec::new();
        for s in guard.values() {
            active_rows.extend(
                s.engine
                    .status_snapshot()
                    .map_err(|e| ApiError::Internal(format!("sender status snapshot: {e:#}")))?,
            );
        }
        (started, active_rows)
    };

    let mut queued = 0u32;
    let mut transferring = 0u32;
    let mut active = Vec::with_capacity(active_rows.len());
    for row in &active_rows {
        match row.state {
            OutboundState::Transferring | OutboundState::Delivered => transferring += 1,
            _ => queued += 1, // Queued / Announced
        }
        active.push(OutboundSummary {
            id: row.id,
            package_short: short_pkg(&row.package_ref),
            state: row.state,
            attempts: row.attempts,
            created_at: row.created_at.clone(),
            peer_short: short_id(&node_id_hex(&row.peer)),
        });
    }

    let (confirmed_total, failed_total) = {
        let db = db(ctx)?;
        let conn = db.conn();
        (count_outbound_state(&conn, "confirmed")?, count_outbound_state(&conn, "failed")?)
    };

    Ok(SyncSenderStatus {
        started,
        queued,
        transferring,
        confirmed_total,
        failed_total,
        active,
    })
}

/// Enriched snapshot for the Transfers UI (task M3): pairing summary + send-side
/// rollup + receive-side rollup, all resolved without any network I/O so a
/// 10-second UI poll never hits the hub.
pub async fn get_status(
    ctx: &ServiceContext,
    sync: &SyncRuntime,
    sender: &SyncSenderRuntime,
) -> Result<SyncStatus, ApiError> {
    let dev_pairing_enabled = dev_pairing_enabled(ctx)?;
    let received_total = received_total(ctx)?;
    let transport_started = sync.is_started().await;
    let pairing_ticket = sync.ticket().await;
    let sender_status = build_sender_status(ctx, sender).await?;

    Ok(SyncStatus {
        dev_pairing_enabled,
        transport_started,
        pairing_ticket,
        received_total,
        sender: sender_status,
        receiver: SyncReceiverStatus { active: transport_started, received_total },
    })
}

/// The transfer history (received + sent), newest first.
pub fn list_history(ctx: &ServiceContext, query: SyncHistoryQuery) -> Result<Vec<HistoryRow>, ApiError> {
    let limit = if query.limit == 0 { DEFAULT_HISTORY_LIMIT } else { query.limit };
    let q = HistoryQuery {
        filename: query.filename,
        object: query.object,
        direction: query.direction,
        peer: query.peer,
        limit,
    };
    let db = db(ctx)?;
    let conn = db.conn();
    search_history_rows(&conn, &q).map_err(|e| ApiError::Internal(format!("{e:#}")))
}

/// Map of node-id-hex → hub device name, for enriching history rows (the rows
/// store the peer node id hex as the stable key; the name is display-only).
/// Best-effort: a hub that is unreachable or a signed-out device yields an empty
/// map (logged at `debug`, never an error) — the UI falls back to short hex.
pub async fn get_sync_device_names(
    ctx: &ServiceContext,
) -> Result<HashMap<String, String>, ApiError> {
    let devices = match crate::api::account::list_devices(ctx).await {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!(error = %format!("{e:?}"), "device names unavailable; falling back to hex");
            return Ok(HashMap::new());
        }
    };
    let mut map = HashMap::new();
    for d in devices {
        if let Ok(id) = pairing::node_id_from_pubkey_b64(&d.pubkey) {
            map.insert(node_id_hex(&id), d.name.clone());
        }
    }
    Ok(map)
}

// ── App sender engine + explicit-target send (sync 2C) ───────────────────────
//
// An app enqueues its own frames to an explicitly chosen destination device
// through a running sender-side [`SyncEngine`], the counterpart of the
// receiver's [`SyncRuntime`]. The engine for a given peer is built lazily on the
// first enqueue to it and cached in the host `AppState`'s [`SyncSenderRuntime`]
// per-peer map; the orchestration lives here (not in `sync::sender`) because it
// owns the account/device + iroh plumbing.

/// Resolve an account device id → its [`NodeId`] via the account device list —
/// the send-side counterpart of the receiver's allow-list resolver. Fetches the
/// hub's device list, finds the device with `id == device_id`, and decodes its
/// base64 `pubkey` into a node id. Errors (all [`ApiError::Invalid`], surfaced
/// to the UI) when the device is absent, its pubkey is undecodable, or — per
/// spec §10 — it is a send-only Perseus agent (never a valid destination: a
/// Perseus node has no receiver, so a package sent to it would never land).
pub async fn resolve_dest_node(ctx: &ServiceContext, device_id: &str) -> Result<NodeId, ApiError> {
    let devices = crate::api::account::list_devices(ctx).await?;
    let Some(device) = devices.iter().find(|d| d.id == device_id) else {
        return Err(ApiError::Invalid(format!(
            "destination device {device_id} is not in the account's device list"
        )));
    };
    if device.capability == crate::account::DeviceCapability::Perseus {
        return Err(ApiError::Invalid(format!(
            "device {device_id} is a send-only Perseus agent and cannot receive frames"
        )));
    }
    pairing::node_id_from_pubkey_b64(&device.pubkey).map_err(|e| {
        ApiError::Invalid(format!("destination device {device_id} has an invalid pubkey: {e:#}"))
    })
}

/// The directory the app writes outgoing packages into (`<sync_dir>/packages`).
fn sender_packages_dir(ctx: &ServiceContext) -> Result<PathBuf, ApiError> {
    let (sync_dir, _db_path) = sync_paths(ctx)?;
    Ok(sync_dir.join("packages"))
}

/// Ensure the sender engine for `dest` is running and return its handle + this
/// device's origin id. Idempotent per destination: a peer that already has a
/// started engine short-circuits without building a transport. The first call
/// for a given `dest` resolves the relay map, binds the shared device identity's
/// iroh transport (attaching `dest`'s dial hint), opens the catalog-backed
/// store, and spawns the engine — then inserts it under `dest` in the per-peer
/// map (sync 2C).
///
/// The runtime mutex is held across the whole build so two concurrent enqueues
/// to the SAME peer can never spawn two engines for it (the second blocks, then
/// sees the populated entry).
///
/// `emitter` is the host's progress sink (Tauri/SSE); it is captured at spawn so
/// the engine's per-package state transitions surface as `sync-progress` /
/// `sync-finished` events for the Transfers UI (task M3). It is only used on the
/// very first (spawning) call for `dest` — subsequent enqueues reuse the cached
/// engine and its captured emitter (a process-global sink, identical regardless
/// of caller).
pub async fn ensure_sender_engine(
    ctx: &ServiceContext,
    sender: &SyncSenderRuntime,
    dest: NodeId,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<(Arc<SyncEngineHandle>, String), ApiError> {
    let mut guard = sender.lock_inner().await;
    if let Some(started) = guard.get(&dest) {
        return Ok((Arc::clone(&started.engine), started.origin_device.clone()));
    }

    let peer = dest;
    let (relay_mode, relay_urls) = resolve_relay_mode(ctx).await?;
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

    // The sender's blob store is DISTINCT from the receiver's (`blobs`). Both
    // halves can run in one process (dev-flag / loopback-validation configs;
    // production role-gating otherwise keeps them mutually exclusive), and a
    // second `FsStore` over the receiver's live dir would either fail on the
    // redb lock or — worse — the sender's startup `delete_all` sweep would wipe
    // the receiver's live `pkg/<id>` tags. A separate `blobs_out` dir keeps the
    // two stores fully independent. (Receiver keeps `blobs` — existing primary
    // deployments already have receiver data there.)
    let transport = crate::sharing::iroh::IrohTransport::new(
        secret,
        relay_mode,
        crate::sharing::iroh::BlobStore::Fs(sync_dir.join("blobs_out")),
    )
    .await
    .map_err(|e| ApiError::Internal(format!("build iroh transport for sender: {e:#}")))?;
    let origin_device = node_id_hex(&transport.node_id());

    // The destination is an account-resolved bare node id (from
    // `resolve_dest_node`). `IrohTransport` has no discovery services, so without
    // a dial hint `announce` fails instantly with "No addressing information
    // available". Attach our own resolved relay URL(s) — the same ones this
    // endpoint itself binds with — as the peer's dial hint before the first
    // announce ever goes out (devices on one account share the hub's relay set).
    let peer_addr = pairing::peer_addr_with_relays(peer, &relay_urls)
        .map_err(|e| ApiError::Internal(format!("construct peer address: {e:#}")))?;
    transport.add_peer(peer_addr);

    let transport: Arc<dyn SharingTransport> = Arc::new(transport);

    let store = Arc::new(
        CatalogSyncStore::open(&db_path)
            .map_err(|e| ApiError::Internal(format!("open catalog sync store: {e:#}")))?,
    );
    let engine = Arc::new(SyncEngine::spawn_with_emitter(
        store as Arc<dyn SyncStore>,
        transport,
        peer,
        emitter,
    ));

    tracing::info!(peer = %node_id_hex(&peer), origin = %origin_device, "sync sender engine started");
    guard.insert(
        dest,
        StartedSender {
            engine: Arc::clone(&engine),
            origin_device: origin_device.clone(),
            peer,
        },
    );
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
    // Per-eligible source linkage recorded into `sync_sources` after the package
    // is written: (catalog file_id, absolute path, size, mtime_ms). This is what
    // retention (task M4) later joins on to resolve a confirmed package back to
    // the disk files it may reclaim, with the recorded stat as the TOCTOU guard.
    let mut source_links: Vec<(i64, String, u64, i64)> = Vec::new();

    for (file_id, file, frame) in &rows {
        let Some(frame_id) = frame.id else { continue };
        resolved.insert(frame_id);

        let path = Path::new(&file.path);
        if !path.exists() {
            ineligible.push(IneligibleFrame { frame_id, reason: "file missing on disk".to_string() });
            continue;
        }
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                ineligible.push(IneligibleFrame { frame_id, reason: format!("cannot stat file: {e}") });
                continue;
            }
        };
        let byte_size = meta.len();
        let mtime_ms = crate::api::retention::mtime_millis(meta.modified().ok());
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
        source_links.push((*file_id, file.path.clone(), byte_size, mtime_ms));
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

    // Record the package → source-file linkage for retention (task M4). Written
    // AFTER the package exists (so a failed write never leaves a dangling
    // linkage) and keyed on the same `package_ref` the engine stores in
    // `sync_outbound`. Best-effort: a failure here only means retention can't
    // reclaim these files later (they stay on disk — the safe direction), so it
    // is logged, never fatal to the send.
    let pkg_ref = pkg_dir.to_string_lossy();
    for (file_id, path, size, mtime_ms) in &source_links {
        if let Err(e) =
            crate::sync::insert_sync_source(conn, &pkg_ref, Some(*file_id), path, *size, *mtime_ms)
        {
            tracing::warn!(error = %e, path = %path, "failed to record sync_sources retention linkage");
        }
    }

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

/// Explicit-target send (sync 2C): enqueue exactly the eligible frames in the
/// selection to the destination peer `dest` as ONE package. Ineligible frames
/// come back in the result. The destination is resolved by the caller (via
/// [`resolve_dest_node`]) and passed in explicitly — this function no longer
/// resolves any implicit "paired primary".
///
/// An empty selection is a benign no-op checked FIRST (task M2 review minor):
/// there is nothing to send, so it must never start an engine for `dest` just
/// because the caller happened to pass zero ids.
pub async fn enqueue_sync_selection(
    ctx: &ServiceContext,
    sender: &SyncSenderRuntime,
    dest: NodeId,
    frame_ids: Vec<i64>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<EnqueueSelectionResult, ApiError> {
    if frame_ids.is_empty() {
        return Ok(EnqueueSelectionResult {
            enqueued_count: 0,
            eligible_count: 0,
            total_count: 0,
            ineligible: Vec::new(),
        });
    }
    let (engine, origin_device) = ensure_sender_engine(ctx, sender, dest, emitter).await?;
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

    /// Sync 2C, task 3: the sender runtime holds ONE engine per destination
    /// peer, addressed by that peer's [`NodeId`]. `current_for` resolves a
    /// present peer to its engine and `None` for an unknown one; `started_peers`
    /// enumerates exactly the addressed peers. Proves the single-`Option` slot
    /// was replaced by a per-peer map.
    #[tokio::test]
    async fn sender_runtime_holds_one_engine_per_peer() {
        use crate::sharing::loopback::LoopbackNetwork;
        use crate::sharing::SharingTransport;
        use crate::sync::StandaloneSyncStore;

        let tmp = tempfile::tempdir().unwrap();
        let node_id_from_hex = |h: &str| crate::sync::node_id_from_hex(h).unwrap();
        // A loopback-backed `StartedSender` (the same shape `ensure_sender_engine`
        // builds), keyed to `peer`. The engine idles with nothing enqueued.
        let fake_started = |peer: NodeId, db_name: &str| {
            let net = LoopbackNetwork::new();
            let store = Arc::new(StandaloneSyncStore::open(tmp.path().join(db_name)).unwrap());
            let engine = Arc::new(SyncEngine::spawn(
                store as Arc<dyn SyncStore>,
                Arc::new(net.endpoint()) as Arc<dyn SharingTransport>,
                peer,
            ));
            StartedSender { engine, origin_device: node_id_hex(&peer), peer }
        };

        let sender = SyncSenderRuntime::new();
        let a: NodeId = node_id_from_hex(&"aa".repeat(32));
        let b: NodeId = node_id_from_hex(&"bb".repeat(32));
        {
            let mut g = sender.lock_inner().await;
            g.insert(a, fake_started(a, "a.db"));
            g.insert(b, fake_started(b, "b.db"));
        }
        assert!(sender.current_for(&a).await.is_some());
        assert!(sender.current_for(&b).await.is_some());
        let c: NodeId = node_id_from_hex(&"cc".repeat(32));
        assert!(sender.current_for(&c).await.is_none(), "unknown peer has no engine");
        assert_eq!(sender.started_peers().await.len(), 2);
    }

    /// The receiver's allow-list is now every device in the account (mesh model,
    /// finding H1 as updated for sync Phase 1) — regardless of a device's
    /// capability (full Athenaeum peer or send-only Perseus agent).
    #[test]
    fn account_peer_hexes_includes_every_device_regardless_of_role() {
        use base64::Engine;
        use crate::account::{AccountDevice, DeviceCapability};
        let b64 = |bytes: [u8; 32]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let dev = |seed: u8, capability: DeviceCapability| AccountDevice {
            id: format!("dev-{seed}"),
            name: format!("n{seed}"),
            pubkey: b64([seed; 32]),
            capability,
            created_at: "2026-07-11T00:00:00Z".into(),
            last_seen_at: None,
        };
        let devices = vec![
            dev(1, DeviceCapability::Athenaeum),
            dev(2, DeviceCapability::Perseus),
            dev(3, DeviceCapability::Athenaeum),
        ];
        let hexes = account_peer_hexes(&devices);
        assert_eq!(hexes.len(), 3, "every account device is authorized, not just paired captures");
        assert!(hexes.contains(&"01".repeat(32)));
        assert!(hexes.contains(&"02".repeat(32)));
        assert!(hexes.contains(&"03".repeat(32)));
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

    /// Task 11: `get_sync_device_names` is best-effort — a signed-out device
    /// (no stored token → `list_devices` errors before any network call) must
    /// resolve to an EMPTY map, never surface an error. The hub URL is pointed
    /// at a bogus host so the keychain/file token lookup finds nothing and the
    /// path is fully hermetic (no network).
    #[tokio::test]
    async fn device_names_empty_when_signed_out() {
        let (_tmp, ctx) = test_ctx();
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            crate::db::set_setting(&conn, keys::ACCOUNT_HUB_URL, "http://sync-test.invalid").unwrap();
        }
        let names = get_sync_device_names(&ctx).await.unwrap();
        assert!(names.is_empty(), "a signed-out device resolves to an empty map, not an error");
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

    /// Task M4: building a selection package records the retention linkage in
    /// `sync_sources` — one live row per eligible frame, keyed on the SAME
    /// `package_ref` the engine stores in `sync_outbound`, carrying the catalog
    /// `file_id` + the file's `(size, mtime)`. This is exactly what
    /// `api::retention` later resolves to reclaim the source. Ineligible frames
    /// (missing on disk) never get a linkage row.
    #[test]
    fn build_selection_writes_sync_sources_linkage() {
        let (tmp, ctx) = test_ctx();
        let dir = tmp.path();
        let f1 = insert_fixture_frame(&ctx, dir, "light-0001.fits", "M42", false);
        let f2 = insert_fixture_frame(&ctx, dir, "light-0002.fits", "M42", false);
        std::fs::remove_file(dir.join("light-0002.fits")).unwrap(); // f2 → missing on disk

        let pkg_root = tmp.path().join("packages");
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let built = build_selection_package(&conn, "origin-dev", &pkg_root, &[f1, f2]).unwrap();
        let pkg_ref = built.pkg_dir.clone().expect("a package was written").to_string_lossy().to_string();

        let sources = crate::sync::live_sources_for_package(&conn, &pkg_ref).unwrap();
        assert_eq!(sources.len(), 1, "one linkage row for the single eligible frame (f2 was missing)");
        let row = &sources[0];
        assert_eq!(row.path, dir.join("light-0001.fits").to_string_lossy(), "linkage points at the source path");
        assert!(row.file_id.is_some(), "the catalog file_id is recorded for a catalog-consistent delete");
        assert_eq!(row.size, std::fs::metadata(dir.join("light-0001.fits")).unwrap().len(), "recorded size matches disk");
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

    /// Sync 2C: an empty selection is a benign no-op checked BEFORE
    /// `ensure_sender_engine` — it returns the zero result and never builds an
    /// engine for `dest`, even though a valid destination node id is supplied.
    /// (The destination is now resolved by the caller and passed in explicitly,
    /// so the send path itself no longer touches the hub; this pins that an
    /// empty selection still short-circuits before any transport build.)
    #[tokio::test]
    async fn enqueue_with_empty_selection_returns_zero_result_without_starting_engine() {
        let (_tmp, ctx) = test_ctx();
        let sender = SyncSenderRuntime::new();
        let dest: NodeId = [7u8; 32];

        let result = enqueue_sync_selection(&ctx, &sender, dest, Vec::new(), None).await.unwrap();
        assert_eq!(result.enqueued_count, 0);
        assert_eq!(result.eligible_count, 0);
        assert_eq!(result.total_count, 0);
        assert!(result.ineligible.is_empty());
        assert!(!sender.is_started().await, "no engine started for an empty selection");
        assert!(sender.started_peers().await.is_empty(), "no peer engine for an empty selection");
    }

    // ── Status enrichment (task M3) ──────────────────────────────────────────

    /// `list_history` applies the new `direction` / `peer` filters SQL-side,
    /// including the two combined.
    #[test]
    fn list_history_filters_by_direction_and_peer() {
        let (_tmp, ctx) = test_ctx();
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let mk = |uuid: &str, dir: Direction, peer: &str, outcome: &str| HistoryRow {
                frame_uuid: uuid.into(),
                filename: format!("{uuid}.fits"),
                object: Some("M42".into()),
                peer_device: peer.into(),
                direction: dir,
                bytes: 100,
                started_at: "2026-07-06T00:00:00.000Z".into(),
                finished_at: None,
                outcome: outcome.into(),
            };
            let ins = crate::sync::store::insert_history_row;
            ins(&conn, &mk("s1", Direction::Sent, "peerA", "sent")).unwrap();
            ins(&conn, &mk("r1", Direction::Received, "peerB", "ingested")).unwrap();
            ins(&conn, &mk("s2", Direction::Sent, "peerB", "confirmed")).unwrap();
        }
        let q = |direction, peer| SyncHistoryQuery {
            filename: None,
            object: None,
            direction,
            peer,
            limit: 0,
        };

        let sent = list_history(&ctx, q(Some(Direction::Sent), None)).unwrap();
        assert_eq!(sent.len(), 2);
        assert!(sent.iter().all(|h| h.direction == Direction::Sent));

        let peer_b = list_history(&ctx, q(None, Some("peerB".into()))).unwrap();
        assert_eq!(peer_b.len(), 2);
        assert!(peer_b.iter().all(|h| h.peer_device == "peerB"));

        let combined = list_history(&ctx, q(Some(Direction::Sent), Some("peerB".into()))).unwrap();
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].frame_uuid, "s2");
    }

    // ── Production account-mode autostart (task A7 fix-review; sync 2A) ─────
    //
    // Bug: a signed-in node without the dev flag never started the receiver
    // (Perseus in production account mode resolved its peer + relay map fine,
    // enqueued, then `serve/announce failed` forever because the app-side node
    // wasn't listening). `autostart_gate` + `account_signed_in` are the exact
    // local-state-only condition; sync Phase 2A drops the old `role == Primary`
    // gate so any signed-in mesh node autostarts. `allow_default_relays` is the
    // companion fix for the addendum (dev flag must never put a signed-in node
    // on iroh's public relays, mixing relay networks with an account-mode peer).

    /// The condition matrix `autostart_if_enabled` gates on: dev flag alone
    /// starts it, signed-in alone starts it, both starts it, neither does not.
    #[test]
    fn autostart_gate_matrix() {
        assert!(autostart_gate(true, false), "dev flag alone must start");
        assert!(autostart_gate(false, true), "signed-in alone must start");
        assert!(autostart_gate(true, true), "both true still starts");
        assert!(!autostart_gate(false, false), "neither condition: must not start");
    }

    /// Sync Phase 2A, task 2: the gate opens for ANY signed-in node — no
    /// `role == Primary` required. (The pure `dev || signed_in` shape is
    /// unchanged; the behavioral broadening lives in the caller, exercised by
    /// `autostart_starts_when_signed_in_without_primary_role`.)
    #[test]
    fn autostart_gate_starts_for_any_signed_in_node() {
        // dev flag alone starts (unchanged).
        assert!(autostart_gate(true, false));
        // signed in (any Athenaeum node) starts — no role required.
        assert!(autostart_gate(false, true));
        // neither → no autostart.
        assert!(!autostart_gate(false, false));
    }

    /// `account_signed_in` derives "is this node signed in" purely from the
    /// persisted `ACCOUNT_DEVICE_ID` (never the hub, never the keychain): no
    /// identity yet → false; identity present → true (the mesh model has no
    /// per-device role — sync 2C removed it entirely); clearing the identity
    /// (sign-out) revokes it.
    #[test]
    fn account_signed_in_matrix() {
        let (_tmp, ctx) = test_ctx();
        assert!(!account_signed_in(&ctx).unwrap(), "no identity yet -> not signed in");

        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            crate::db::set_setting(&conn, keys::ACCOUNT_DEVICE_ID, "device-1").unwrap();
        }
        assert!(account_signed_in(&ctx).unwrap(), "identity present -> signed in");

        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            crate::db::delete_setting(&conn, keys::ACCOUNT_DEVICE_ID).unwrap();
        }
        assert!(!account_signed_in(&ctx).unwrap(), "identity cleared (sign-out) -> not signed in");
    }

    /// `autostart_if_enabled`'s one remaining negative path never touches relay
    /// resolution or the transport: signed-out + dev off returns `Ok(false)`
    /// with the receiver never started (fast — no hub, no iroh). (A signed-in
    /// node — of any role — is no longer a negative path as of sync Phase 2A;
    /// it now passes the gate, covered by
    /// `autostart_starts_when_signed_in_without_primary_role`.)
    #[tokio::test]
    async fn autostart_signed_out_dev_off_never_starts_the_receiver() {
        use crate::sync::SyncRuntime;

        let (_tmp, ctx) = test_ctx();
        let sync = SyncRuntime::new();

        let started = autostart_if_enabled(&ctx, &sync, Arc::new(crate::events::NullEmitter))
            .await
            .unwrap();
        assert!(!started, "signed-out + dev off must not start");
        assert!(!sync.is_started().await);
    }

    /// Sync Phase 2A, task 2: a signed-in node with NO role set must pass the
    /// autostart gate — in the mesh model every signed-in Athenaeum node is a
    /// full peer and runs a receiver, with no `role == Primary` gate.
    ///
    /// We assert at the gate boundary (the receiver never short-circuits at the
    /// early `Ok(false)`) rather than on a fully-`Ok(true)` transport: this ctx
    /// has no hub creds and no cached relay map, so once the gate admits, the
    /// call deterministically proceeds into `resolve_relay_mode` and fails there
    /// (`relay_mode_for(&[], allow_default = false)`), never reaching the real
    /// iroh transport that `ensure_started` would bind. The whole autostart test
    /// suite is hermetic-by-design (no sockets); the behavioral change under
    /// test is purely "the gate is no longer role-gated". Old code returned the
    /// early `Ok(false)` here (its gate was `dev || role==Primary`, both false),
    /// so this genuinely fails RED before the fix.
    #[tokio::test]
    async fn autostart_starts_when_signed_in_without_primary_role() {
        use crate::sync::SyncRuntime;

        let (_tmp, ctx) = test_ctx();
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            // Signed in (identity present) — the mesh model has no role gate.
            crate::db::set_setting(&conn, keys::ACCOUNT_DEVICE_ID, "device-1").unwrap();
        }
        let sync = SyncRuntime::new();
        let started =
            autostart_if_enabled(&ctx, &sync, Arc::new(crate::events::NullEmitter)).await;
        assert!(
            !matches!(started, Ok(false)),
            "a signed-in node must pass the autostart gate regardless of role \
             (must not short-circuit at Ok(false)); got {started:?}"
        );
    }

    /// Fix-review addendum: a signed-in device must never get the dev-only
    /// Default-relay fallback, even with the dev flag on — the app-side node
    /// must stay on the account's hub relay map so an account-mode peer
    /// (Perseus) can still dial it. Only a signed-OUT session (pure dev/ticket
    /// mode) may fall back to Default.
    #[test]
    fn allow_default_relays_signed_in_always_refuses_even_with_dev_flag() {
        assert!(!allow_default_relays(true, true), "signed-in + dev flag: still refused");
        assert!(!allow_default_relays(true, false), "signed-in, no dev flag: refused");
        assert!(allow_default_relays(false, true), "signed-out + dev flag: pure dev/ticket mode allowed");
        assert!(!allow_default_relays(false, false), "signed-out, no dev flag: refused");
    }

    /// Fix-review addendum, pinned end to end: dev flag ON + signed in + the
    /// hub answers with a real relay map → the resolved `RelayMode` uses the
    /// hub's relays (`Custom`), never `Default`. This is the exact production
    /// scenario that broke — toggling the dev flag as a troubleshooting
    /// workaround while ALSO signed in must not put the app on iroh's public
    /// n0 relays while an account-mode peer sits on the hub's relay map
    /// (different relay networks → dial-by-node-id fails instantly).
    #[tokio::test]
    async fn signed_in_with_dev_flag_on_still_prefers_hub_relay_map_over_default() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/v1/relay-map"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "relays": ["https://relay1.example.org"]
            })))
            .mount(&server)
            .await;

        let res = pairing::resolve_relays(Some((server.uri().as_str(), "tok")), &[]).await;
        assert_eq!(res.urls, vec!["https://relay1.example.org".to_string()]);
        assert!(res.fresh, "a live hub answer is fresh");

        // Signed in (creds present) AND the dev flag on: the Default fallback
        // must be refused regardless — composing exactly what `resolve_relay_mode`
        // does internally.
        let allow_default = allow_default_relays(true, true);
        assert!(!allow_default, "a signed-in device must never get the Default-relay opt-in");

        let mode = pairing::relay_mode_for(&res.urls, allow_default).unwrap();
        assert!(
            matches!(mode, iroh::RelayMode::Custom(_)),
            "must use the hub's relay map, not iroh's public defaults, got {mode:?}"
        );
    }
}
