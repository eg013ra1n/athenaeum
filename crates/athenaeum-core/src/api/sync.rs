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
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::api::frame_set_send::PayloadEntry;
use crate::api::{db, ApiError};
use crate::events::ProgressEmitter;
use crate::package::{self, ManifestRecord, PayloadKind, MANIFEST_VERSION};
use crate::services::ServiceContext;
use crate::settings::{defaults, keys};
use crate::sharing::iroh::node::{RelayResolver, Role, SharedIrohNode};
use crate::sharing::types::{NodeId, PackageLayout};
use crate::sharing::SharingTransport;
use crate::sync::engine::SERVE_ACTIVITY_FRESHNESS;
use crate::sync::status::outbound_display_state;
use crate::sync::store::{
    get_inbound_by_row_id, inbound_active, inbound_file_counts, list_inbound_files,
    list_outbound_files, list_sync_events, outbound_file_counts, outbound_row_by_id,
    search_history_rows, terminal_inbound, terminal_outbound,
};
use crate::sync::{
    node_id_hex, pairing, CatalogSyncStore, Direction, HistoryQuery, HistoryRow, InboundRow,
    InboundSummary, OutboundRow, OutboundState, OutboundSummary, QueuedInboundSummary,
    RefusalRefresher, StartedSender, SyncEngine, SyncEngineHandle, SyncReceiverStatus, SyncRuntime,
    SyncSenderRuntime, SyncSenderStatus, SyncStatus, SyncStore, TransferFileCounts,
    TransferFileEntry, TransportHealth, CANCELLED_BY_RECEIVER_DETAIL,
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
    /// Exact `project_id` filter (Stage II collab); unfiltered when absent. The
    /// Transfers UI's project-dimension passthrough (Task 11).
    pub project: Option<String>,
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
    let v = ctx.settings.get_with_precedence(
        &conn,
        keys::SYNC_DEV_TICKET_PAIRING,
        defaults::SYNC_DEV_TICKET_PAIRING,
    )?;
    Ok(v.eq_ignore_ascii_case("true"))
}

/// Resolve the sync data dir (`<db_parent>/sync`) and the catalog DB path.
/// Everything sync needs — device key, blob store, landed files — lives beside
/// the catalog so it follows the same OS-appdata / Docker `/data` location.
///
/// `pub(crate)` so the collab request-to-serve path
/// ([`crate::api::collab_exchange`]) resolves the same sync dir + catalog path
/// its dedicated `blobs_collab` sender engine binds under.
pub(crate) fn sync_paths(ctx: &ServiceContext) -> Result<(PathBuf, PathBuf), ApiError> {
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
    Ok(raw
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
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
    devices
        .iter()
        .filter_map(|d| pubkey_b64_to_hex(&d.pubkey))
        .collect()
}

/// A node-id-hex → current-device-name map over the account device list — the
/// cached source the receiver reads to name an incoming sender's landing folder
/// by that sender's CURRENT friendly name (with no per-package hub round-trip).
/// Undecodable pubkeys are skipped; the same map `get_sync_device_names` builds,
/// but persisted so the receiver can resolve offline. Serialized to
/// [`SYNC_DEVICE_NAMES`](keys::SYNC_DEVICE_NAMES) by [`refresh_authorized_peers`].
fn account_device_names(devices: &[crate::account::AccountDevice]) -> HashMap<String, String> {
    devices
        .iter()
        .filter_map(|d| pubkey_b64_to_hex(&d.pubkey).map(|hex| (hex, d.name.clone())))
        .collect()
}

/// A node-id-hex → device-capability (`"athenaeum"` / `"perseus"`) map over the
/// account device list — the capability sibling of [`account_device_names`]. The
/// receiver reads this cache at announce time to stamp the announcing peer's
/// capability onto its `sync_inbound` row (Perseus UI v2, Task 9), so the
/// Transfers UI can show whether a received transfer came from a full Athenaeum
/// peer or a send-only Perseus agent. Undecodable pubkeys are skipped. Serialized
/// to [`SYNC_PEER_CAPABILITIES`](keys::SYNC_PEER_CAPABILITIES) by
/// [`refresh_authorized_peers`].
fn account_device_capabilities(
    devices: &[crate::account::AccountDevice],
) -> HashMap<String, String> {
    devices
        .iter()
        .filter_map(|d| {
            pubkey_b64_to_hex(&d.pubkey).map(|hex| (hex, d.capability.as_str().to_string()))
        })
        .collect()
}

/// Build the receiver's live peer-authorization gate (finding H1). A signed-in
/// node enforces the cached allow-list (`SYNC_AUTHORIZED_PEERS`), re-read from
/// settings on **every** announce so a hub refresh takes effect on the next
/// package. A pure dev-ticket node (no account) has no hub to build a list from,
/// so it accepts any peer — the dev flag is a developer-only escape hatch.
///
/// Fail closed: a signed-in node whose cache is empty (never refreshed)
/// authorizes nobody until [`refresh_authorized_peers`] populates it.
fn peer_authorizer(
    ctx: &Arc<ServiceContext>,
    refusal: Arc<RefusalRefresher>,
) -> Result<crate::sync::PeerAuthorizer, ApiError> {
    if !account_signed_in(ctx)? {
        tracing::warn!(
            "sync receiver has no account allow-list (dev-ticket mode); accepting any peer"
        );
        return Ok(crate::sync::allow_all_peers());
    }
    let db = db(ctx)?.clone();
    let ctx = Arc::clone(ctx);
    Ok(Arc::new(move |from: &crate::sharing::types::NodeId| {
        let hex = crate::sync::node_id_hex(from);
        let authorized = {
            let conn = db.conn();
            match crate::db::get_setting(&conn, keys::SYNC_AUTHORIZED_PEERS) {
                Ok(Some(raw)) => raw.lines().map(str::trim).any(|line| line == hex),
                _ => false, // fail closed: no list yet ⇒ authorize nobody
            }
        };
        // Refusing an unknown peer is a hint our cached set is stale (task 7):
        // kick a debounced hub refresh so a machine just added to the account is
        // admitted on the peer's next retry — no callback to the refused peer, its
        // own retry loop redelivers.
        if !authorized {
            maybe_refresh_on_refusal(&refusal, &ctx, &hex);
        }
        authorized
    }))
}

// ── collab exchange receiver hooks (slice 4) ─────────────────────────────────

/// The receiver's connection-level connect gate (collab exchange, slice 4).
/// A signed-in node admits a dialing peer when it is EITHER in the account
/// device allow-list (`SYNC_AUTHORIZED_PEERS`, the same idiom as the H1
/// per-package authorizer) OR a verified member of ANY cached collaboration
/// project ([`collab::authz::node_in_any_project`](crate::collab::authz::node_in_any_project)).
/// Both checks are re-read live per connection, so a hub / membership-snapshot
/// refresh takes effect on the next dial without a transport restart.
///
/// Fail-closed: a signed-in node with empty caches admits nobody. A node with no
/// account (pure dev-ticket mode) installs NO gate — accept-all, the same
/// developer escape hatch as [`peer_authorizer`]'s [`crate::sync::allow_all_peers`].
fn connect_gate(
    ctx: &Arc<ServiceContext>,
    refusal: Arc<RefusalRefresher>,
) -> Result<Option<crate::sharing::iroh::ConnectGate>, ApiError> {
    if !account_signed_in(ctx)? {
        return Ok(None);
    }
    let db = db(ctx)?.clone();
    let ctx = Arc::clone(ctx);
    Ok(Some(Arc::new(move |from: &NodeId| {
        let hex = crate::sync::node_id_hex(from);
        let admit = {
            let conn = db.conn();
            let in_account = match crate::db::get_setting(&conn, keys::SYNC_AUTHORIZED_PEERS) {
                Ok(Some(raw)) => raw.lines().map(str::trim).any(|line| line == hex),
                _ => false,
            };
            in_account || crate::collab::authz::node_in_any_project(&conn, from)
        };
        // Same debounced-refresh hint as the per-announce authorizer (task 7): an
        // unknown dialer we admit nobody-of may be a freshly-added account device.
        if !admit {
            maybe_refresh_on_refusal(&refusal, &ctx, &hex);
        }
        admit
    })))
}

/// The receiver's per-announce project-membership gate (collab exchange,
/// slice 4): an inbound `ProjectAnnounceReceived` is accepted only from a
/// verified current member of that project
/// ([`collab::authz::may_accept_announce`](crate::collab::authz::may_accept_announce)),
/// re-read live per announce. Always installed and fail-closed by construction —
/// a node with no matching membership row simply drops every project announce.
fn project_announce_gate(
    ctx: &ServiceContext,
) -> Result<crate::sync::ProjectAnnounceGate, ApiError> {
    let db = db(ctx)?.clone();
    Ok(Arc::new(move |from: &NodeId, project_id: &str| {
        let conn = db.conn();
        crate::collab::authz::may_accept_announce(&conn, project_id, from)
    }))
}

/// Assemble the slice-4 [`ReceiverHooks`](crate::sync::ReceiverHooks) that both
/// `ensure_started` callers pass: the composite connect gate (installed only when
/// signed in) plus the always-on project announce gate, the task-6 holder-side
/// serve handler that answers inbound project pull requests, and the task-8
/// announcements-refresh + post-ingest report-have hooks.
fn receiver_hooks(
    ctx: &Arc<ServiceContext>,
    refusal: Arc<RefusalRefresher>,
    request_handler: Option<crate::sync::ProjectRequestHandler>,
) -> Result<crate::sync::ReceiverHooks, ApiError> {
    Ok(crate::sync::ReceiverHooks {
        connect_gate: connect_gate(ctx, refusal)?,
        project_gate: Some(project_announce_gate(ctx)?),
        announcements_refresher: Some(announcements_refresher(Arc::clone(ctx))),
        on_project_ingested: Some(on_project_ingested_hook(Arc::clone(ctx))),
        project_request_handler: request_handler,
        // W2 T2.7: the persisted receive-concurrency cap travels with the hooks
        // because `sync::` has no `ServiceContext` to read it from — see the
        // field's doc. `ensure_started` applies it before the loop spawns.
        max_concurrent_receives: configured_max_concurrent_receives(ctx),
    })
}

/// The persisted `sync.max_concurrent_receives`, for the receiver about to
/// start (W2 T2.7). `None` when the value cannot be read at all — the receiver
/// then keeps [`DEFAULT_MAX_CONCURRENT_RECEIVES`](crate::sync::DEFAULT_MAX_CONCURRENT_RECEIVES).
///
/// A settings read must never block startup: an unreachable DB or a garbage row
/// degrades this to "receive at the shipped cap", never to "no receiver". Out-of-
/// range rows don't even reach here as errors — the getter clamps them into
/// 1..=8.
fn configured_max_concurrent_receives(ctx: &ServiceContext) -> Option<usize> {
    match db(ctx).and_then(|d| {
        let conn = d.conn();
        ctx.settings
            .get_sync_max_concurrent_receives(&conn)
            .map_err(|e| ApiError::Internal(e.to_string()))
    }) {
        Ok(n) => Some(n),
        Err(e) => {
            tracing::warn!(
                error = %e,
                default = crate::sync::DEFAULT_MAX_CONCURRENT_RECEIVES,
                "read receive concurrency cap at receiver start failed; using the default"
            );
            None
        }
    }
}

/// Build the task-8 [`ProjectAnnouncementsRefresher`](crate::sync::ProjectAnnouncementsRefresher)
/// the receiver invokes when an inbound project announce names a package whose hub
/// row we don't yet know. The hook is synchronous by contract, so it
/// `tokio::spawn`s the async hub poll (the house pattern — see
/// [`project_request_handler`]) and returns immediately; the receive loop never
/// blocks. The receiver's immediate re-check may miss the still-in-flight poll,
/// but the sender's announce retry lands once the row appears.
fn announcements_refresher(ctx: Arc<ServiceContext>) -> crate::sync::ProjectAnnouncementsRefresher {
    Arc::new(move |project_id: &str| {
        let ctx = Arc::clone(&ctx);
        let project_id = project_id.to_string();
        tokio::spawn(async move {
            if let Err(e) =
                crate::api::collab_exchange::refresh_project_packages(&ctx, &project_id).await
            {
                tracing::warn!(project_id = %project_id, error = %format!("{e}"), "announcements refresh failed");
            }
        });
    })
}

/// Build the task-8 [`ProjectIngestedHook`](crate::sync::ProjectIngestedHook)
/// fired after a project package ingests + acks: seed the package (D3 T4), then a
/// best-effort report-have so the hub adds this device to the package's swarm.
/// `tokio::spawn`ed off the receive loop; a failure only means the hub doesn't
/// list us as a holder yet.
///
/// Seed BEFORE report-have (D3 §3.4): `report_have` is what makes other members'
/// swarm fetches dial this device, so advertising first would publish a holder
/// whose blobs are not servable yet. The seed is best-effort — a failed seed still
/// reports have, because a landed package is still servable through the on-demand
/// `handle_project_request` path (exactly the pre-D3 semantics).
fn on_project_ingested_hook(ctx: Arc<ServiceContext>) -> crate::sync::ProjectIngestedHook {
    Arc::new(move |project_id: String, package_id: String| {
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            crate::api::collab_exchange::seed_ingested_package(&ctx, &package_id).await;
            if let Err(e) =
                crate::api::collab_exchange::report_have_after_ingest(&ctx, &package_id).await
            {
                tracing::warn!(project_id = %project_id, package_id = %package_id, error = %format!("{e}"), "post-ingest report_have failed");
            }
        });
    })
}

/// Build the holder-side [`ProjectRequestHandler`](crate::sync::ProjectRequestHandler)
/// the receiver invokes on an inbound `ProjectRequestReceived` (task 6). The
/// closure is `'static` — it captures a cloned `Arc<ServiceContext>`, the
/// host-owned collab sender map, and the host emitter — and `tokio::spawn`s
/// [`handle_project_request`](crate::api::collab_exchange::handle_project_request)
/// so the synchronous receive loop never blocks on the serve. An authorization
/// failure inside `handle_project_request` is a silent (warn-logged) drop; a real
/// error is logged here.
///
/// `collab_sender` is the DEDICATED collab sender map (a SECOND
/// [`SyncSenderRuntime`](crate::sync::SyncSenderRuntime), distinct from the
/// personal-sync `sync_sender` — collab serves ride a dedicated `blobs_collab`
/// store, audit m7). Task 11 hoists it to `AppState.collab_sender` /
/// `WebAppState.collab_sender` so the Transfers UI can roll up collab transfers;
/// both host state constructors build it beside `sync_sender`.
fn project_request_handler(
    ctx: Arc<ServiceContext>,
    collab_sender: Arc<SyncSenderRuntime>,
    emitter: Arc<dyn ProgressEmitter>,
) -> crate::sync::ProjectRequestHandler {
    Arc::new(
        move |from: NodeId, project_id: String, package_id: String| {
            let ctx = Arc::clone(&ctx);
            let sender = Arc::clone(&collab_sender);
            let emitter = Arc::clone(&emitter);
            tokio::spawn(async move {
                if let Err(e) = crate::api::collab_exchange::handle_project_request(
                    &ctx,
                    &sender,
                    from,
                    project_id,
                    package_id,
                    Some(emitter),
                )
                .await
                {
                    tracing::error!(error = %format!("{e:#}"), "collab request-to-serve failed");
                }
            });
        },
    )
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
    let names = account_device_names(&devices);
    let capabilities = account_device_capabilities(&devices);
    if let Ok(db) = db(ctx) {
        let conn = db.conn();
        if let Err(e) =
            crate::db::set_setting(&conn, keys::SYNC_AUTHORIZED_PEERS, &hexes.join("\n"))
        {
            tracing::warn!(error = %e, "failed to cache authorized peers");
        } else {
            tracing::info!(count = hexes.len(), "refreshed authorized account peers");
        }
        // Cache the hex → device-name map too (best-effort, cosmetic): the receiver
        // reads it to name incoming senders' landing folders by their current
        // friendly name without a per-package hub round-trip. A serialize/write
        // failure only degrades to hex-slug folders — never blocks the allow-list.
        match serde_json::to_string(&names) {
            Ok(json) => {
                if let Err(e) = crate::db::set_setting(&conn, keys::SYNC_DEVICE_NAMES, &json) {
                    tracing::warn!(error = %e, "failed to cache device names");
                } else {
                    tracing::debug!(count = names.len(), "refreshed cached device names");
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to serialize device names cache"),
        }
        // Cache the hex → device-capability map too (best-effort, informational):
        // the receiver reads it at announce time to stamp the announcing peer's
        // capability onto its inbound row (Perseus UI v2, Task 9), which is how the
        // Transfers UI later shows "from a Perseus agent" vs "from an Athenaeum
        // peer". A serialize/write failure only leaves rows unstamped — never
        // blocks the allow-list.
        match serde_json::to_string(&capabilities) {
            Ok(json) => {
                if let Err(e) = crate::db::set_setting(&conn, keys::SYNC_PEER_CAPABILITIES, &json) {
                    tracing::warn!(error = %e, "failed to cache device capabilities");
                } else {
                    tracing::debug!(
                        count = capabilities.len(),
                        "refreshed cached device capabilities"
                    );
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to serialize device capabilities cache"),
        }
    }
}

/// How often the periodic timer re-pulls the authorized-device set from the hub
/// (task 7). One hour bounds cache staleness without polling the hub hot; the
/// refusal-triggered path ([`maybe_refresh_on_refusal`]) covers the fast case (a
/// machine just added to the account), so the timer is only the slow backstop.
const PEERS_REFRESH_INTERVAL: Duration = Duration::from_secs(3600);

/// Install (once per process) the hourly authorized-peers refresh timer (task 7).
/// The [`SyncRuntime::peers_refresh_task`] slot is the guard: the first caller
/// spawns the loop and stashes its handle; every later call sees `Some` and
/// no-ops, so all three startup sites (autostart / pairing-ticket disclosure /
/// first sender-engine build) can call this and whichever runs first wins.
///
/// The loop drives a [`tokio::time::interval`]; its FIRST tick fires immediately
/// (tokio semantics), which is a harmless extra refresh — the startup path already
/// ran one — so it is left in rather than skipped. Each tick calls the existing
/// best-effort [`refresh_authorized_peers`] (warns-and-keeps the cached set on any
/// hub/credential failure). The `peers_refresh_task` lock is released before any
/// `.await` inside the loop — it is only held to check-and-stamp the slot here.
async fn ensure_peers_refresh_task(ctx: &Arc<ServiceContext>, sync: &SyncRuntime) {
    let mut guard = sync.peers_refresh_task.lock().await;
    if guard.is_some() {
        return;
    }
    let ctx = Arc::clone(ctx);
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(PEERS_REFRESH_INTERVAL);
        loop {
            ticker.tick().await;
            refresh_authorized_peers(&ctx).await;
        }
    });
    *guard = Some(handle);
    tracing::debug!(
        interval_secs = PEERS_REFRESH_INTERVAL.as_secs(),
        "authorized-peers refresh timer installed"
    );
}

/// On refusing an UNKNOWN peer from either receiver gate, kick a debounced hub
/// refresh of the authorized set (task 7). Shared process-wide by the one
/// [`RefusalRefresher`] on [`SyncRuntime`], so a refusal burst across both gates
/// triggers at most one hub round-trip per gap. The refused peer's own retry loop
/// redelivers once its device row lands in our cache — no callback to it is
/// needed. Synchronous by construction (the gates are sync closures): it only
/// stamps the debounce and `tokio::spawn`s the async refresh, never blocking the
/// gate.
fn maybe_refresh_on_refusal(refusal: &Arc<RefusalRefresher>, ctx: &Arc<ServiceContext>, hex: &str) {
    if refusal.should_fire() {
        tracing::info!(peer = %hex, "unknown peer refused; refreshing authorized set");
        let ctx = Arc::clone(ctx);
        tokio::spawn(async move {
            refresh_authorized_peers(&ctx).await;
        });
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
pub(crate) async fn resolve_relay_mode(
    ctx: &ServiceContext,
) -> Result<(iroh::RelayMode, Vec<String>), ApiError> {
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

// ── relay-map lifecycle wiring (iroh hardening T8, H2) ───────────────────────

/// Build the node's relay-map resolver (H2): the hub-agnostic callback the node's
/// hourly refresh loop re-runs to learn the CURRENT relay map. It re-runs
/// [`resolve_relay_mode`] (which re-fetches + re-caches the hub relay map), so a
/// relay-map change the hub publishes is picked up and drives an idle node
/// rebuild. Captures an owned `Arc<ServiceContext>` so it can run inside the
/// node's detached refresh task; a resolve error yields `None` (the loop keeps the
/// current relay map).
fn node_relay_resolver(ctx: Arc<ServiceContext>) -> RelayResolver {
    Arc::new(move || {
        let ctx = Arc::clone(&ctx);
        Box::pin(async move {
            match resolve_relay_mode(&ctx).await {
                Ok((mode, urls)) => Some((mode, urls)),
                Err(e) => {
                    tracing::warn!(error = %format!("{e}"), "relay refresh: resolve failed; keeping current relay map");
                    None
                }
            }
        })
    })
}

/// Start the node's hourly relay-map refresh loop (H2). Idempotent — the node
/// no-ops a second call — so every entry point that binds/uses the node can call
/// it (autostart, dev-ticket disclosure, first sender-engine build) and whichever
/// runs first wins.
fn start_node_relay_refresh(ctx: Arc<ServiceContext>, node: &Arc<SharedIrohNode>) {
    node.start_relay_refresh(node_relay_resolver(ctx));
}

/// Install the node's transport-level wake hook (T6, sync delivery-forever). On a
/// home-relay **reconnect** transition or an **applied relay-map change** the node
/// fires this hook; it kicks every pending outbound package — personal AND collab
/// — out of its backoff so it re-announces the instant the node is reachable
/// again, instead of waiting out the exponential retry window.
///
/// The node is a process singleton and every start entry point installs the SAME
/// two host-global sender maps, so `set_wake_hook`'s last-writer-wins is benign
/// (each install is an identical hook). The closure is `'static` — it owns cloned
/// `Arc<SyncSenderRuntime>` handles — and `tokio::spawn`s the fire-and-forget
/// `kick_all` fan-out so the hook itself returns promptly (it runs on the node's
/// relay-watcher / refresh task, which must not block).
fn install_node_wake_hook(
    ctx: &Arc<ServiceContext>,
    node: &Arc<SharedIrohNode>,
    sync_sender: Arc<SyncSenderRuntime>,
    collab_sender: Arc<SyncSenderRuntime>,
) {
    let ctx = Arc::clone(ctx);
    let node_for_beacon = Arc::clone(node);
    node.set_wake_hook(Arc::new(move || {
        let sync_sender = Arc::clone(&sync_sender);
        let collab_sender = Arc::clone(&collab_sender);
        let ctx = Arc::clone(&ctx);
        let node = Arc::clone(&node_for_beacon);
        tokio::spawn(async move {
            sync_sender.kick_all().await;
            collab_sender.kick_all().await;
            // Edge 2 of 2 (D1 §3.1): our relay just reconnected, which is the same
            // fact as "we are reachable again" — so tell the peers that may be
            // parked waiting for us. Costs one beacon per account device per
            // reconnect, and only ever on a transition.
            broadcast_presence(&ctx, &node).await;
        });
    }));
}

/// Debounce window for INBOUND presence beacons, per peer (D1 §3.2). A peer whose
/// relay flaps would otherwise fan a burst of beacons into a burst of attempts.
const PRESENCE_DEBOUNCE: Duration = Duration::from_secs(10);

/// How many presence beacons a fan-out keeps in flight (D1 §3.1). The fan-out
/// runs at startup across every account device, so it must not serialize behind
/// switched-off machines — nor open an unbounded number of connections at once.
const PRESENCE_FANOUT_CONCURRENCY: usize = 4;

/// Last time we ACTED on a presence beacon from each peer (the debounce state).
static PRESENCE_SEEN: std::sync::OnceLock<std::sync::Mutex<HashMap<NodeId, std::time::Instant>>> =
    std::sync::OnceLock::new();

/// Whether this beacon is outside the debounce window, stamping it if so.
fn presence_debounce_admits(peer: NodeId) -> bool {
    let map = PRESENCE_SEEN.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("presence debounce mutex poisoned");
    let now = std::time::Instant::now();
    match guard.get(&peer) {
        Some(last) if now.duration_since(*last) < PRESENCE_DEBOUNCE => false,
        _ => {
            guard.insert(peer, now);
            true
        }
    }
}

/// Whether we hold at least one non-terminal outbound row for `peer` — i.e.
/// whether a presence beacon from it has anything to resume.
fn has_pending_rows_for(ctx: &ServiceContext, peer: NodeId) -> bool {
    let Ok((_sync_dir, db_path)) = sync_paths(ctx) else {
        return false;
    };
    let Ok(store) = CatalogSyncStore::open(&db_path) else {
        return false;
    };
    match store.non_terminal() {
        Ok(rows) => rows.iter().any(|r| r.peer == peer),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "presence: pending-row lookup failed");
            false
        }
    }
}

/// Handle one inbound presence beacon (D1 §3.2): ensure-then-kick, behind two
/// gates evaluated from LOCAL state only.
///
/// The gates matter because this is a remote message that ALLOCATES: without the
/// first, any node that can reach our control channel could make us build sender
/// engines; without the second, a beacon from an idle device would build one for
/// nothing. Neither gate consults the network, so a beacon is cheap to refuse.
///
/// Ensure-then-kick rather than kick: a beacon that beats
/// `resurrect_pending_senders` — or lands on a signed-in device whose cached
/// allow-list was empty when resurrection ran, so every peer was filtered out —
/// would otherwise find no engine and be silently wasted, which is exactly the
/// case the beacon exists for.
pub async fn handle_peer_presence(
    ctx: &Arc<ServiceContext>,
    sender: &Arc<SyncSenderRuntime>,
    collab_sender: &Arc<SyncSenderRuntime>,
    sync: &SyncRuntime,
    emitter: Option<Arc<dyn ProgressEmitter>>,
    from: NodeId,
) {
    let hex = node_id_hex(&from);
    if !cached_authorized_peer_hexes(ctx).contains(&hex) {
        tracing::warn!(peer = %hex, "presence from an unauthorized peer; ignored");
        return;
    }
    if !has_pending_rows_for(ctx, from) {
        tracing::debug!(peer = %hex, "presence from an authorized peer with nothing pending");
        return;
    }
    if !presence_debounce_admits(from) {
        tracing::debug!(peer = %hex, "presence within the debounce window; ignored");
        return;
    }
    if sender.current_for(&from).await.is_none() {
        if let Err(e) = ensure_sender_engine(
            ctx,
            sender,
            Arc::clone(collab_sender),
            sync,
            from,
            // No reported address needed: the beacon just proved the peer is
            // dialable on the path it arrived by.
            None,
            emitter,
        )
        .await
        {
            tracing::warn!(peer = %hex, error = %format!("{e:#}"), "presence: engine build failed");
            return;
        }
    }
    tracing::info!(peer = %hex, "peer presence; resuming its queue");
    sender.kick_peer(&from).await;
    collab_sender.kick_peer(&from).await;
}

/// Announce our presence to every authorized ACCOUNT device (D1 §3.1).
///
/// Account devices only: collab holders belong to other accounts, and telling
/// them when we come online would leak a presence signal nobody asked for. Our
/// own devices learn nothing they are not already entitled to know.
///
/// Fire-and-forget by contract — a beacon that does not land costs nothing,
/// because the sender's retry schedule is the fallback for every peer that never
/// hears one.
pub async fn broadcast_presence(ctx: &Arc<ServiceContext>, node: &Arc<SharedIrohNode>) {
    let peers = cached_authorized_peer_hexes(ctx);
    let self_id = node.node_id();
    // The node binds with `presets::Minimal` (no discovery), so a bare node id is
    // NOT dialable: without a hint `send_presence` has nothing to connect to. The
    // send path registers an address per peer when it builds an engine
    // (`ensure_sender_engine`), but a beacon goes out from the RECEIVING half to
    // devices we may never have sent to — so the fan-out has to supply its own
    // hint, or every beacon dies locally with "no known address". Our own relay
    // set is the right hint: same-account devices home onto the same hub-served
    // relays, which is why `peer_addr_with_relays` exists.
    let relay_urls = node.relay_urls();
    if relay_urls.is_empty() {
        tracing::debug!("presence fan-out skipped: no relays configured");
        return;
    }
    let mut tasks = tokio::task::JoinSet::new();
    let mut sent = 0usize;
    for hex in peers {
        let Ok(peer) = crate::sync::node_id_from_hex(&hex) else {
            continue;
        };
        if peer == self_id {
            continue; // never beacon ourselves
        }
        match pairing::peer_addr_with_relays(peer, &relay_urls) {
            Ok(addr) if !addr.is_empty() => node.add_peer(addr),
            Ok(_) => continue,
            Err(e) => {
                tracing::debug!(peer = %hex, error = %format!("{e:#}"), "presence: dial hint build failed");
                continue;
            }
        }
        let node = Arc::clone(node);
        tasks.spawn(async move {
            if let Err(e) = node.send_presence(peer).await {
                tracing::debug!(peer = %hex, error = %format!("{e:#}"), "presence beacon undelivered");
            }
        });
        sent += 1;
        while tasks.len() >= PRESENCE_FANOUT_CONCURRENCY {
            let _ = tasks.join_next().await;
        }
    }
    while tasks.join_next().await.is_some() {}
    if sent > 0 {
        tracing::info!(count = sent, "presence beacon fan-out complete");
    }
}

/// Install the inbound-presence hook and fire the first beacon (D1 §3.1/§3.2).
///
/// Called from the same three sites as [`install_node_wake_hook`], so a
/// sender-first bind is wired exactly like a receiver-first one. Installing is
/// idempotent (the last hook wins and they are identical); the beacon is fired on
/// a detached task so no startup path waits on the network.
fn install_presence_wiring(
    ctx: &Arc<ServiceContext>,
    node: &Arc<SharedIrohNode>,
    sync: &Arc<SyncRuntime>,
    sync_sender: &Arc<SyncSenderRuntime>,
    collab_sender: &Arc<SyncSenderRuntime>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) {
    {
        let ctx = Arc::clone(ctx);
        let sync = Arc::clone(sync);
        let sync_sender = Arc::clone(sync_sender);
        let collab_sender = Arc::clone(collab_sender);
        let emitter = emitter.clone();
        node.set_presence_hook(Arc::new(move |from| {
            let ctx = Arc::clone(&ctx);
            let sync = Arc::clone(&sync);
            let sync_sender = Arc::clone(&sync_sender);
            let collab_sender = Arc::clone(&collab_sender);
            let emitter = emitter.clone();
            // The hook runs on the control accept loop: hand the work to its own
            // task so a beacon can never stall the loop that receives announces.
            tokio::spawn(async move {
                handle_peer_presence(&ctx, &sync_sender, &collab_sender, &sync, emitter, from)
                    .await;
            });
        }));
    }
    // Edge 1 of 2: we just came online. (Edge 2 — our own relay reconnecting —
    // rides the wake hook.)
    let ctx = Arc::clone(ctx);
    let node = Arc::clone(node);
    tokio::spawn(async move {
        broadcast_presence(&ctx, &node).await;
    });
}

/// Build the T8 retry-time peer-address refresher for a personal-sync sender
/// engine: on a timed-out retry it re-fetches the peer's CURRENT hub-reported
/// endpoint address (a fresh `list_devices`) plus fresh relay urls, and returns
/// the merged [`pairing::peer_dial_addr`] so the re-attempt dials the peer's
/// current path instead of a cached-stale one. `None` (peer gone / hub blip)
/// leaves the address the transport already knows in place. Same account, so
/// `cross_account = false`.
fn sender_addr_refresher(ctx: Arc<ServiceContext>) -> crate::sync::engine::AddrRefresher {
    Arc::new(move |peer: NodeId| {
        let ctx = Arc::clone(&ctx);
        Box::pin(async move {
            let relay_urls = match resolve_relay_mode(&ctx).await {
                Ok((_, urls)) => urls,
                Err(e) => {
                    tracing::warn!(error = %format!("{e}"), "retry addr refresh: relay resolve failed");
                    return None;
                }
            };
            // The peer's CURRENT hub-reported address (may have moved relays).
            let reported = match crate::api::account::list_devices(&ctx).await {
                Ok(devices) => devices
                    .into_iter()
                    .find_map(|d| match pairing::node_id_from_pubkey_b64(&d.pubkey) {
                        Ok(id) if id == peer => Some(d.endpoint_addr),
                        _ => None,
                    })
                    .flatten(),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:?}"), "retry addr refresh: device list unavailable");
                    None
                }
            };
            match pairing::peer_dial_addr(peer, reported.as_ref(), &relay_urls, false) {
                Ok(addr) => Some(addr),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "retry addr refresh: address build failed");
                    None
                }
            }
        })
    })
}

/// Lazily bind (once per process) the ONE [`SharedIrohNode`] and stash it on
/// [`ServiceContext::iroh_node`] (C1 fix, Д2). The first caller — whichever of
/// the receiver / personal sender / collab sender needs a transport first —
/// resolves the relay mode ONCE, binds the single endpoint + store at
/// `<sync>/blobs`, cleans up the now-orphaned per-role stores, and stores the
/// node; every later caller reuses it. A re-bind after
/// [`SharedIrohNode::shutdown`] is allowed (the `Option` is cleared at
/// host-shutdown), so this can bind again on a subsequent boot within one
/// process (tests).
///
/// The `iroh_node` mutex is held across the whole async bind so two concurrent
/// first callers can't bind two endpoints from the same device key (the second
/// blocks, then sees the populated slot). It is never held while acquiring the
/// `SyncRuntime`/`SyncSenderRuntime` locks, so there is no lock-ordering cycle.
pub(crate) async fn ensure_iroh_node(
    ctx: &ServiceContext,
) -> Result<Arc<SharedIrohNode>, ApiError> {
    let mut guard = ctx.iroh_node.lock().await;
    if let Some(node) = guard.as_ref() {
        return Ok(Arc::clone(node));
    }
    let (relay_mode, _relay_urls) = resolve_relay_mode(ctx).await?;
    let (sync_dir, _db_path) = sync_paths(ctx)?;
    std::fs::create_dir_all(&sync_dir)
        .map_err(|e| ApiError::Internal(format!("create sync dir {}: {e}", sync_dir.display())))?;
    let node = SharedIrohNode::bind(&sync_dir, relay_mode)
        .await
        .map_err(|e| ApiError::Internal(format!("bind shared iroh node: {e:#}")))?;
    // W1: the node always binds unlimited, so the persisted device-wide upload
    // cap is applied right here. This ONE site covers desktop AND web — the node
    // binds lazily in core, so neither host needs startup code of its own. A
    // settings read that fails must never fail the bind: log it and run
    // unlimited (transfers degraded to "not throttled", never to "no transport").
    match db(ctx).and_then(|d| {
        let conn = d.conn();
        ctx.settings
            .get_sync_max_upload_bytes_per_sec(&conn)
            .map_err(|e| ApiError::Internal(e.to_string()))
    }) {
        Ok(bytes_per_sec) => node.set_upload_limit(bytes_per_sec),
        Err(e) => tracing::warn!(
            error = %e,
            "read upload limit at bind failed; running unlimited"
        ),
    }
    // Unified store: the node binds at `<sync>/blobs`. After the first successful
    // bind, the old per-role stores are dead weight — remove them so a migrated
    // install doesn't carry three parallel blob DBs (tolerate absence).
    cleanup_orphan_blob_stores(&sync_dir);
    // Report THIS device's dialable endpoint address to the hub (finding H1, T7):
    // a fire-and-forget task that polls the node's address and PUTs it on change.
    // Only when signed in (a pure dev-ticket node has no hub to report to); never
    // blocks the bind — spawned and detached, self-terminating on node drop.
    if let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx).ok().flatten() {
        pairing::spawn_endpoint_address_reporter(Arc::clone(&node), hub_url, token);
    }
    *guard = Some(Arc::clone(&node));
    Ok(node)
}

/// Delete the now-orphaned per-role blob-store directories left by the
/// pre-Task-3 split transports (`blobs_out` for the personal sender,
/// `blobs_collab` for the collab sender). The shared node's single store lives
/// at `<sync>/blobs`, so these two siblings hold nothing live once every role
/// rides the node. Best-effort: a missing dir is the common (fresh-install)
/// case and is silent; any other error is logged, never fatal.
fn cleanup_orphan_blob_stores(sync_dir: &Path) {
    for name in ["blobs_out", "blobs_collab"] {
        let dir = sync_dir.join(name);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => tracing::info!(path = %dir.display(), "removed orphaned per-role blob store"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                path = %dir.display(),
                error = %e,
                "failed to remove orphaned per-role blob store"
            ),
        }
    }
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
    ctx: Arc<ServiceContext>,
    sync: Arc<SyncRuntime>,
    sync_sender: Arc<SyncSenderRuntime>,
    collab_sender: Arc<SyncSenderRuntime>,
    emitter: Arc<dyn ProgressEmitter>,
) -> Result<bool, ApiError> {
    // Own the Arc (the request-to-serve handler clones it into a `'static`
    // closure) and borrow it for the rest of the body unchanged.
    let ctx_arc = ctx;
    let ctx: &ServiceContext = &ctx_arc;
    let dev = dev_pairing_enabled(ctx)?;
    let signed_in = account_signed_in(ctx)?;
    if !autostart_gate(dev, signed_in) {
        return Ok(false);
    }
    tracing::debug!(dev, signed_in, "sync autostart condition met");
    let (sync_dir, db_path) = sync_paths(ctx)?;
    let incoming = incoming_resolver(ctx, sync_dir.join("incoming"))?;
    // Lazily bind the ONE shared node (resolves the relay mode + binds the single
    // endpoint/store on first need; the receiver rides it as its Recv role
    // handle). The receiver only listens, so no dial-hint relay URLs are needed.
    let node = ensure_iroh_node(ctx).await?;
    // Bound relay-map staleness (T8, H2): start the node's hourly relay refresh
    // loop at boot (idempotent).
    start_node_relay_refresh(Arc::clone(&ctx_arc), &node);
    // Wake event → kick pending packages (T6): relay reconnect / relay-map change
    // fans a fire-and-forget kick_all over the personal + collab sender maps.
    install_node_wake_hook(
        &ctx_arc,
        &node,
        Arc::clone(&sync_sender),
        Arc::clone(&collab_sender),
    );
    // Peer reachability (D1): install the inbound-beacon hook and fire our own
    // first beacon — edge 1 of 2, "we just came online".
    install_presence_wiring(
        &ctx_arc,
        &node,
        &sync,
        &sync_sender,
        &collab_sender,
        Some(Arc::clone(&emitter)),
    );
    // Periodic authorized-peers refresh (task 7): install the hourly hub re-pull
    // so a machine added to the account later is admitted without a restart
    // (idempotent — once per process across all three startup sites).
    ensure_peers_refresh_task(&ctx_arc, &sync).await;
    // Populate the authorized-peer allow-list (best-effort) before the receiver
    // starts accepting, then enforce it live per-package (finding H1).
    refresh_authorized_peers(ctx).await;
    let authorized = peer_authorizer(&ctx_arc, Arc::clone(&sync.refusal))?;
    let hooks = receiver_hooks(
        &ctx_arc,
        Arc::clone(&sync.refusal),
        Some(project_request_handler(
            Arc::clone(&ctx_arc),
            Arc::clone(&collab_sender),
            Arc::clone(&emitter),
        )),
    )?;
    // Clone the emitter for the resurrection + auto-sync spawns before
    // `ensure_started` moves it.
    let emitter_for_resurrect = Arc::clone(&emitter);
    let emitter_for_auto_sync = Arc::clone(&emitter);
    sync.ensure_started(
        node, sync_dir, db_path, incoming, authorized, hooks, emitter,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    // tv2 follow-up: resurrect orphaned sender engines for every non-terminal
    // outbound row the previous process left behind, so a pending send resumes
    // (and becomes cancellable + visible) after a restart. Spawned fire-and-forget
    // AFTER the receiver is up so autostart latency is unchanged; idempotent via
    // the per-peer `current_for` guard, so racing a concurrent user enqueue never
    // double-spawns an engine.
    tokio::spawn(resurrect_pending_senders(
        Arc::clone(&ctx_arc),
        Arc::clone(&sync_sender),
        Arc::clone(&collab_sender),
        Arc::clone(&sync),
        emitter_for_resurrect,
    ));
    // B7: reclaim leftover transfer temp data (row-less payload dirs + orphaned
    // receiver in-flight blob tags). Fire-and-forget AFTER `ensure_started`, same
    // placement as the resurrection spawn; race-safe against it by construction
    // (see `sweep_transfer_orphans`), so autostart latency is unchanged.
    tokio::spawn(sweep_transfer_orphans(
        Arc::clone(&ctx_arc),
        Arc::clone(&sync),
    ));
    // D3 §3.3: arm the collab auto-replication worker — published contributions
    // of every auto-enabled project download themselves. Armed here (and at the
    // other `ensure_started` site) rather than in each host's startup: the pass
    // pulls through `download_project_package`, which needs exactly the started
    // `SyncRuntime` this function just produced. Idempotent — the FIRST call
    // spawns the one loop and every later one no-ops.
    crate::api::collab_exchange::spawn_collab_auto_sync(
        Arc::clone(&ctx_arc),
        Arc::clone(&sync),
        Some(emitter_for_auto_sync),
    );
    Ok(true)
}

/// The pure condition [`autostart_if_enabled`] gates on: the dev pairing flag,
/// OR this device being signed in (any Athenaeum node — no role gate). Factored
/// out for a fast, network-free unit test of the exact matrix (task A7
/// fix-review, broadened in sync Phase 2A).
fn autostart_gate(dev: bool, signed_in: bool) -> bool {
    dev || signed_in
}

/// "Is device-to-device sync configured on this node" — the SAME predicate the
/// receiver's autostart uses, deliberately shared rather than re-derived.
///
/// `files.content_hash` has two consumers: the transfer dedup handshake, and
/// content-based grouping in the Duplicates view when
/// `duplicates.use_content_hash` is on. Only the first is worth 3 x 512 KB of
/// disk reads per catalogued file UNPROMPTED, so the content-index job's
/// AUTOMATIC trigger is gated on this predicate; the manual start in Settings
/// is not gated at all, which is how a node that only wants the duplicate
/// grouping fills the column. Local state only — no hub call, no keychain.
pub fn sync_configured(ctx: &ServiceContext) -> Result<bool, ApiError> {
    Ok(autostart_gate(
        dev_pairing_enabled(ctx)?,
        account_signed_in(ctx)?,
    ))
}

/// The DISTINCT destination peers of a set of non-terminal outbound rows,
/// first-seen order preserved (tv2 follow-up — sender resurrection). One engine
/// per peer re-drives every one of that peer's non-terminal rows, so the startup
/// resurrection only needs the distinct peer set, not the row set. Pure over the
/// store read so the enumeration is unit-testable without the transport build.
fn distinct_pending_peers(rows: &[OutboundRow]) -> Vec<NodeId> {
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut peers = Vec::new();
    for row in rows {
        if seen.insert(row.peer) {
            peers.push(row.peer);
        }
    }
    peers
}

/// Restrict a resurrection peer set to same-account devices (tv2 follow-up,
/// review fix). `sync_outbound` has no personal/project discriminator — the
/// dedicated collab sender writes the very same table — so
/// [`distinct_pending_peers`] can enumerate a peer whose only pending rows are
/// COLLAB/project rows, including a cross-account collaborator. Resurrection
/// only ever builds a personal-role engine (see [`resurrect_pending_senders`]),
/// which such a peer's own receiver would refuse: not a corruption, but a
/// pointless forever-retry loop this filter avoids by construction. A peer not
/// present in `authorized_hex` (the cached account-device allow-list) is
/// dropped and logged at `debug!` — expected/routine, never `warn!`. Pure and
/// iroh-free so the exact filtering logic is unit-testable without a transport.
///
/// Building a COLLAB-role engine for a collab-only orphaned row is deliberately
/// OUT of scope here — deferred to the collab cycle; this fix's job is only to
/// stop the wrong (personal) engine from being built for such a peer.
fn filter_account_peers(peers: Vec<NodeId>, authorized_hex: &HashSet<String>) -> Vec<NodeId> {
    peers
        .into_iter()
        .filter(|peer| {
            let hex = node_id_hex(peer);
            let ok = authorized_hex.contains(&hex);
            if !ok {
                tracing::debug!(
                    peer = %hex,
                    "resurrection skipped: not an account device (likely a collab-only row)"
                );
            }
            ok
        })
        .collect()
}

/// The cached account-device allow-list ([`keys::SYNC_AUTHORIZED_PEERS`]) as a
/// hex-`NodeId` set — the exact same local cache [`peer_authorizer`] enforces
/// per-announce, populated by [`refresh_authorized_peers`] (never a hub call
/// itself; read-only local-DB peek). `ServiceContext`-unavailable or unset reads
/// as an empty set (fail-closed, matching `peer_authorizer`'s own fail-closed
/// default): a signed-in node whose cache hasn't populated yet resurrects
/// nothing until the next refresh, rather than guessing.
fn cached_authorized_peer_hexes(ctx: &ServiceContext) -> HashSet<String> {
    let Ok(db) = db(ctx) else {
        return HashSet::new();
    };
    let conn = db.conn();
    match crate::db::get_setting(&conn, keys::SYNC_AUTHORIZED_PEERS) {
        Ok(Some(raw)) => raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        _ => HashSet::new(),
    }
}

/// Resurrect orphaned sender engines after an app restart (tv2 follow-up).
///
/// The ONLY production `SyncEngine` spawn is [`ensure_sender_engine`], reached
/// from `enqueue_sync_selection` / `retry_sync_package`. After a restart the
/// in-memory per-peer engine map is empty while non-terminal `sync_outbound` rows
/// persist, so those rows are orphaned: no engine → no worker re-drive → no
/// re-announce, they are invisible in `get_status().sender.active` (which iterates
/// started engines), and `cancel_sync_package` fails "package is not active". This
/// reads the durable non-terminal rows and (re)builds one engine per distinct
/// peer — the spawned worker then re-drives every non-terminal row scoped to that
/// peer, identical to the pre-restart in-process behavior.
///
/// `authorized` gates the peer set to same-account devices via
/// [`filter_account_peers`] — `None` means no gate at all (pure dev-ticket mode
/// has no account concept, so nothing to intersect against; matches
/// [`peer_authorizer`]'s own accept-all in that mode), `Some(hexes)` filters to
/// exactly that cached allow-list (an empty set fails closed: resurrects
/// nothing, same as `peer_authorizer`). Collab-role resurrection for a
/// collab-only orphaned row is deliberately deferred to the collab cycle — this
/// only stops the wrong (personal) engine from being built for such a peer.
///
/// `build` is the per-peer engine ensurer: production
/// ([`resurrect_pending_senders`]) passes [`ensure_sender_engine`]; tests inject a
/// loopback builder (the same substitution the whole `sync_e2e` harness makes —
/// `ensure_sender_engine` binds a real iroh transport that has no loopback
/// stand-in). Best-effort per peer: a build failure (peer unauthorized / hub
/// unreachable) is `warn!` + continue, never propagated — a restart must resurrect
/// every peer it can and never fail on one bad peer. **Recovery boundary**: a
/// failed build is not retried by this function itself — the peer stays orphaned
/// until the next restart (another `resurrect_pending_senders` run) or a manual
/// send to that peer (`enqueue_sync_selection` / `retry_sync_package`, which build
/// their own engine on demand). The `current_for` guard makes it idempotent and
/// race-safe: a peer whose engine already exists (a concurrent user enqueue won
/// the race, or a second startup site already ran) is skipped, so two engines are
/// never spawned for one peer. Returns the number of peers this call *ensured* an
/// engine exists for — under the same cross-site/user-enqueue race the guard
/// makes safe, two concurrent callers can each ensure the same peer (one actually
/// builds, the other's `build` call finds it already there via
/// `ensure_sender_engine`'s own lock and returns `Ok` immediately), so this count
/// can exceed the number of engines truly newly spawned process-wide — it is an
/// "ensured" count, not a strict "newly built" count.
#[doc(hidden)]
pub async fn resurrect_pending_senders_with<F, Fut>(
    db_path: &Path,
    sender: &Arc<SyncSenderRuntime>,
    authorized: Option<&HashSet<String>>,
    build: F,
) -> Result<usize, ApiError>
where
    F: Fn(NodeId) -> Fut,
    Fut: std::future::Future<Output = Result<(), ApiError>>,
{
    let rows = {
        let store = CatalogSyncStore::open(db_path)
            .map_err(|e| ApiError::Internal(format!("open catalog sync store: {e:#}")))?;
        store
            .non_terminal()
            .map_err(|e| ApiError::Internal(format!("read non-terminal outbound rows: {e:#}")))?
    };
    let mut peers = distinct_pending_peers(&rows);
    if let Some(hexes) = authorized {
        peers = filter_account_peers(peers, hexes);
    }
    let mut ensured = 0usize;
    for peer in peers {
        // Idempotent + race-safe: skip a peer whose engine is already running (a
        // concurrent enqueue, or another startup site, beat us to it).
        if sender.current_for(&peer).await.is_some() {
            continue;
        }
        match build(peer).await {
            Ok(()) => ensured += 1,
            Err(e) => {
                // Best-effort: one unbuildable peer never fails the resurrection.
                tracing::warn!(
                    peer = %node_id_hex(&peer),
                    error = %format!("{e:#}"),
                    "resurrect_pending_senders: engine build failed; skipping peer"
                );
            }
        }
    }
    if ensured > 0 {
        tracing::info!(
            peers = ensured,
            rows = rows.len(),
            "resurrected pending sender engines"
        );
    }
    Ok(ensured)
}

/// Production entry point for [`resurrect_pending_senders_with`]: builds each
/// orphaned peer's engine through the real [`ensure_sender_engine`] — exactly the
/// call `retry_sync_package` makes when a restart cleared the engine map while its
/// terminal rows persisted. Best-effort and self-contained (own error logging), so
/// the boot path can `tokio::spawn` it fire-and-forget without awaiting or
/// propagating — autostart latency is unchanged. The first-attempt dial hint falls
/// back to our own relay set (`dest_endpoint_addr = None`); the engine's T8 address
/// refresher re-resolves the peer's real address on retry.
pub async fn resurrect_pending_senders(
    ctx: Arc<ServiceContext>,
    sender: Arc<SyncSenderRuntime>,
    collab_sender: Arc<SyncSenderRuntime>,
    sync: Arc<SyncRuntime>,
    emitter: Arc<dyn ProgressEmitter>,
) {
    let db_path = match sync_paths(&ctx) {
        Ok((_sync_dir, db_path)) => db_path,
        Err(e) => {
            tracing::warn!(
                error = %format!("{e:#}"),
                "resurrect_pending_senders: cannot resolve sync paths; skipping"
            );
            return;
        }
    };
    // Same-account gate (review fix): `sync_outbound` has no personal/project
    // discriminator, so the enumeration can surface a peer whose only pending rows
    // are collab/project rows — resurrection must never build a personal-role
    // engine for a peer outside this account. Pure dev-ticket mode (no account) has
    // no allow-list concept, so it resurrects any pending peer unfiltered — the
    // same accept-all `peer_authorizer` applies in that mode.
    let authorized = match account_signed_in(&ctx) {
        Ok(true) => Some(cached_authorized_peer_hexes(&ctx)),
        _ => None,
    };
    let build = |peer: NodeId| {
        let ctx = Arc::clone(&ctx);
        let sender = Arc::clone(&sender);
        let collab = Arc::clone(&collab_sender);
        let sync = Arc::clone(&sync);
        let emitter = Arc::clone(&emitter);
        async move {
            ensure_sender_engine(&ctx, &sender, collab, &sync, peer, None, Some(emitter))
                .await
                .map(|_| ())
        }
    };
    if let Err(e) =
        resurrect_pending_senders_with(&db_path, &sender, authorized.as_ref(), build).await
    {
        tracing::warn!(
            error = %format!("{e:#}"),
            "resurrect_pending_senders: enumeration failed"
        );
    }
}

/// Dev-flagged: lazily start the transport + receiver and return this device's
/// pairing ticket. `Forbidden` when the dev flag is off. The sync data (device
/// key, blob store, landed files) lives beside the catalog DB, under a `sync/`
/// sibling directory.
pub async fn get_pairing_ticket(
    ctx: Arc<ServiceContext>,
    sync: Arc<SyncRuntime>,
    sync_sender: Arc<SyncSenderRuntime>,
    collab_sender: Arc<SyncSenderRuntime>,
    emitter: Arc<dyn ProgressEmitter>,
) -> Result<String, ApiError> {
    // Own the Arc (the request-to-serve handler clones it into a `'static`
    // closure) and borrow it for the rest of the body unchanged.
    let ctx_arc = ctx;
    let ctx: &ServiceContext = &ctx_arc;
    // Dev-gate first; resolve paths and drop the DB borrow before awaiting.
    if !dev_pairing_enabled(ctx)? {
        return Err(ApiError::Forbidden(
            "personal sync is dev-gated; enable sync.dev_ticket_pairing first".into(),
        ));
    }
    let (sync_dir, db_path) = sync_paths(ctx)?;
    let incoming = incoming_resolver(ctx, sync_dir.join("incoming"))?;
    // Same reasoning as `autostart_if_enabled`: bind the ONE shared node (relay
    // mode resolved inside), which the receiver rides as its Recv role handle.
    let node = ensure_iroh_node(ctx).await?;
    // Bound relay-map staleness (T8, H2): start the node's hourly relay refresh
    // loop (idempotent).
    start_node_relay_refresh(Arc::clone(&ctx_arc), &node);
    // Wake event → kick pending packages (T6): relay reconnect / relay-map change
    // fans a fire-and-forget kick_all over the personal + collab sender maps.
    install_node_wake_hook(
        &ctx_arc,
        &node,
        Arc::clone(&sync_sender),
        Arc::clone(&collab_sender),
    );
    // Peer reachability (D1): install the inbound-beacon hook and fire our own
    // first beacon — edge 1 of 2, "we just came online".
    install_presence_wiring(
        &ctx_arc,
        &node,
        &sync,
        &sync_sender,
        &collab_sender,
        Some(Arc::clone(&emitter)),
    );
    // Periodic authorized-peers refresh (task 7): install the hourly hub re-pull
    // here too (idempotent — no-ops if autostart already installed it).
    ensure_peers_refresh_task(&ctx_arc, &sync).await;
    // Enforce the authorized-peer allow-list here too (finding H1). In pure
    // dev-ticket mode (no account) this resolves to accept-any; a signed-in
    // primary that also flips the dev flag still enforces its account list.
    refresh_authorized_peers(ctx).await;
    let authorized = peer_authorizer(&ctx_arc, Arc::clone(&sync.refusal))?;
    let hooks = receiver_hooks(
        &ctx_arc,
        Arc::clone(&sync.refusal),
        Some(project_request_handler(
            Arc::clone(&ctx_arc),
            Arc::clone(&collab_sender),
            Arc::clone(&emitter),
        )),
    )?;

    // Clone the emitter for the resurrection + auto-sync spawns before
    // `ensure_started` moves it.
    let emitter_for_resurrect = Arc::clone(&emitter);
    let emitter_for_auto_sync = Arc::clone(&emitter);
    let ticket = sync
        .ensure_started(
            node, sync_dir, db_path, incoming, authorized, hooks, emitter,
        )
        .await
        .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    // tv2 follow-up: resurrect orphaned sender engines here too — this is the
    // OTHER startup site that runs `ensure_started` for a dev node. Fire-and-forget
    // AFTER the receiver is up (never blocks the ticket return); idempotent via the
    // per-peer `current_for` guard, so it no-ops if autostart already resurrected.
    tokio::spawn(resurrect_pending_senders(
        Arc::clone(&ctx_arc),
        Arc::clone(&sync_sender),
        Arc::clone(&collab_sender),
        Arc::clone(&sync),
        emitter_for_resurrect,
    ));
    // B7: reclaim leftover transfer temp data here too — the OTHER startup site
    // that runs `ensure_started`. Fire-and-forget, race-safe against resurrection
    // (see `sweep_transfer_orphans`); never blocks the ticket return.
    tokio::spawn(sweep_transfer_orphans(
        Arc::clone(&ctx_arc),
        Arc::clone(&sync),
    ));
    // D3 §3.3: arm the collab auto-replication worker here too — the OTHER
    // `ensure_started` site. Idempotent (once per process), so it no-ops if
    // autostart already armed it.
    crate::api::collab_exchange::spawn_collab_auto_sync(
        Arc::clone(&ctx_arc),
        Arc::clone(&sync),
        Some(emitter_for_auto_sync),
    );
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
        .query_row(
            "SELECT COUNT(*) FROM sync_outbound WHERE state = ?1",
            [state],
            |r| r.get(0),
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(n.max(0) as u32)
}

/// Total frames received (history rows with `direction = received`).
fn received_total(ctx: &ServiceContext) -> Result<u32, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_history WHERE direction = 'received'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(n.max(0) as u32)
}

/// The cached node-id-hex → friendly device-name map (`SYNC_DEVICE_NAMES`), read
/// ONCE per status poll — a plain settings read with NO hub round-trip (§D5/§D8;
/// the same cache the receiver's landing-folder naming reads). Best-effort: an
/// absent setting or malformed JSON yields an empty map (warned, never fatal), so
/// the UI falls back to short hex.
fn cached_device_names(conn: &rusqlite::Connection) -> HashMap<String, String> {
    match crate::db::get_setting(conn, keys::SYNC_DEVICE_NAMES) {
        Ok(Some(raw)) => match serde_json::from_str::<HashMap<String, String>>(&raw) {
            Ok(map) => map,
            Err(e) => {
                tracing::warn!(error = %e, "sync status: device-names cache parse failed; using hex");
                HashMap::new()
            }
        },
        Ok(None) => HashMap::new(),
        Err(e) => {
            tracing::warn!(error = %e, "sync status: device-names cache read failed; using hex");
            HashMap::new()
        }
    }
}

/// The cached node-id-hex → device-capability (`"athenaeum"` / `"perseus"`) map
/// (`SYNC_PEER_CAPABILITIES`), read ONCE per receive-side read — a plain settings
/// read with NO hub round-trip, the capability sibling of [`cached_device_names`]
/// (written by [`refresh_authorized_peers`]). It's the fallback the
/// [`inbound_summary`] mapper consults for a legacy inbound row whose stamped
/// `peer_capability` is NULL. Best-effort: an absent setting or malformed JSON
/// yields an empty map (warned, never fatal), so such a row simply gets no
/// `peer_kind`.
fn cached_device_kind_map(conn: &rusqlite::Connection) -> HashMap<String, String> {
    match crate::db::get_setting(conn, keys::SYNC_PEER_CAPABILITIES) {
        Ok(Some(raw)) => match serde_json::from_str::<HashMap<String, String>>(&raw) {
            Ok(map) => map,
            Err(e) => {
                tracing::warn!(error = %e, "sync status: capability cache parse failed; no peer_kind");
                HashMap::new()
            }
        },
        Ok(None) => HashMap::new(),
        Err(e) => {
            tracing::warn!(error = %e, "sync status: capability cache read failed; no peer_kind");
            HashMap::new()
        }
    }
}

/// Dedup in-flight rows by durable `sync_outbound` id, keeping the first
/// occurrence and ordering ascending by id.
///
/// Every per-peer engine ([`SyncSenderRuntime`], sync 2C) opens its OWN
/// [`CatalogSyncStore`] over the SAME catalog DB, and
/// [`SyncStore::non_terminal`](crate::sync::SyncStore::non_terminal) has no peer
/// filter — so with N started engines the naive rollup returns N copies of every
/// non-terminal row. Collapsing by id yields exactly the distinct in-flight
/// packages, from which `queued`/`transferring` are then counted (never
/// N-inflated). A [`std::collections::BTreeMap`] gives both first-occurrence
/// (`or_insert`) and the stable ascending-by-id ordering the Active tab expects.
/// Deduping the raw rows (not the summaries) means the one-manifest-read-per-row
/// [`package_totals`] cost is paid once per DISTINCT package, not once per engine.
fn dedup_active_rows(rows: Vec<OutboundRow>) -> Vec<OutboundRow> {
    let mut by_id: std::collections::BTreeMap<i64, OutboundRow> = std::collections::BTreeMap::new();
    for row in rows {
        by_id.entry(row.id).or_insert(row);
    }
    by_id.into_values().collect()
}

/// `(file_count, total_bytes)` from a package dir's manifest (Task 14). One
/// manifest read per call — cheap enough for the handful of in-flight rows a
/// 5–10s status poll rolls up. A missing/unreadable manifest (a half-built or
/// since-vanished package dir) yields `(0, 0)` (logged at `debug`, never fatal):
/// a status poll must never fail on one bad package.
fn package_totals(dir: &Path) -> (u32, u64) {
    match package::read_manifest(dir) {
        Ok(records) => (
            records.len() as u32,
            records.iter().map(|r| r.byte_size).sum(),
        ),
        Err(e) => {
            tracing::debug!(path = %dir.display(), error = %format!("{e:#}"), "package_totals: manifest unreadable");
            (0, 0)
        }
    }
}

/// Whether `row_id` is waiting on its peer's serial lane rather than on the
/// network: true iff that peer is pulling something from us right now
/// (`peer_active`) and that something is a DIFFERENT package (variant A).
///
/// The honest-labeling guard lives here: the package actually moving is never
/// described as queued behind anything — it IS the thing everything else is queued
/// behind. `peer_active = None` (idle peer, or the serve signal decayed past
/// [`SERVE_ACTIVITY_FRESHNESS`]) leaves every row on today's behavior.
fn row_receiver_busy(row_id: i64, peer_active: Option<i64>) -> bool {
    matches!(peer_active, Some(active) if active != row_id)
}

/// Map one persisted `sync_outbound` row to its display [`OutboundSummary`] —
/// the SINGLE outbound-row → summary mapping, shared by the live Active-tab
/// rollup ([`build_sender_status`]) and the terminal-rows read
/// ([`list_terminal_transfers`]) so a package presents identically
/// (display_state, display_name, device_name, file_counts, retrying) whether it
/// came from the live poll or the restart-survival read. `device_names` /
/// `file_counts` are the batch-resolved maps the caller reads once; `now` anchors
/// the `waiting`-backoff derivation; `receiver_busy` (variant A) says this row's
/// peer is pulling a SIBLING batch of ours right now, which reads as
/// `queued_at_receiver` instead of a countdown. Only the live rollup can know that
/// (it is the one holding the engines) — [`list_terminal_transfers`] passes
/// `false`, which costs nothing: the display chain never relabels a terminal row.
///
/// `resendable` always comes back `false` here — computing it requires an
/// fs-stat of the package dir ([`package_has_payload`]), which the cheap 10s
/// `get_sync_status` poll must never pay (a live, non-terminal row never shows
/// Resend anyway). Only [`list_terminal_transfers`] overrides it per row, since
/// that read already knows it's dealing with a terminal row whose payload may
/// since have been cleaned up by retention.
fn outbound_summary(
    row: OutboundRow,
    now: chrono::DateTime<chrono::Utc>,
    device_names: &HashMap<String, String>,
    file_counts: &HashMap<i64, TransferFileCounts>,
    receiver_busy: bool,
) -> OutboundSummary {
    let (file_count, byte_size) = package_totals(Path::new(&row.package_ref));
    let peer_hex = node_id_hex(&row.peer);
    let display_state = outbound_display_state(
        row.state,
        row.next_retry_at.as_deref(),
        row.last_error.as_deref(),
        receiver_busy,
        now,
    );
    let stalled_until = if display_state == "waiting" {
        row.next_retry_at.clone()
    } else {
        None
    };
    // The batch identity the UI keys on: the package-dir basename, which B3
    // aligned to equal the wire `batch_uuid` and which `sync_history.package_id`
    // (sent) already carries — so it is exactly the `package_key`
    // `delete_transfer_history` takes for a sent batch.
    let batch_uuid = outbound_package_key(&row.package_ref);
    OutboundSummary {
        id: row.id,
        package_short: short_pkg(&row.package_ref),
        state: row.state,
        attempts: row.attempts,
        generation: row.generation,
        batch_uuid,
        created_at: row.created_at,
        peer_short: short_id(&peer_hex),
        last_error: row.last_error,
        retrying: row.next_retry_at.is_some(),
        next_retry_at: row.next_retry_at,
        byte_size,
        file_count,
        display_name: row.display_name.filter(|s| !s.trim().is_empty()),
        device_name: device_names.get(&peer_hex).cloned(),
        display_state,
        stalled_until,
        file_counts: file_counts.get(&row.id).copied().unwrap_or_default(),
        resendable: false,
    }
}

/// Map one persisted `sync_inbound` row to its display [`InboundSummary`] — the
/// SINGLE inbound-row → summary mapping, shared by the live receive-side Active
/// rollup ([`active_inbound_summaries`]) and the terminal-rows read
/// ([`list_terminal_transfers`]). Inbound presentation state mirrors the raw state
/// but for the two arms below, and there is no receiver-side retry/backoff concept,
/// so `stalled_until` is always `None`.
///
/// `parked_for_slot` is the live set of wire package ids waiting on the receive gate
/// ([`InboundControl::parked_for_slot_snapshot`]); callers with no running receiver
/// — and every terminal-row read, where no entry could match — pass an empty set.
///
/// [`InboundControl::parked_for_slot_snapshot`]: crate::sync::InboundControl::parked_for_slot_snapshot
fn inbound_summary(
    row: InboundRow,
    device_names: &HashMap<String, String>,
    device_kinds: &HashMap<String, String>,
    file_counts: &HashMap<i64, TransferFileCounts>,
    parked_for_slot: &HashSet<String>,
) -> InboundSummary {
    // The stable per-transfer key the receiver keys its one long-lived row on;
    // fall back to the wire package id for a legacy row whose `batch_uuid` is
    // NULL (a v1/v2 receive, or a row created before the column).
    let batch_uuid = row
        .batch_uuid
        .clone()
        .unwrap_or_else(|| row.package_id.clone());
    // Origin capability (Perseus UI v2, Task 10): the value stamped on the row at
    // announce time (Task 9) wins; a legacy row whose stamp is NULL falls back to
    // the cached `SYNC_PEER_CAPABILITIES` hex→kind map keyed on the sending peer.
    let peer_kind = row
        .peer_capability
        .clone()
        .or_else(|| device_kinds.get(&row.peer).cloned());
    InboundSummary {
        id: row.id,
        package_id: row.package_id.clone(),
        package_short: short_id(&row.package_id),
        peer_short: short_id(&row.peer),
        // D2 §3.6: one chip for both directions. The send side derives
        // `waiting_peer` from an error-class prefix; the receive side has it as a
        // stored state. Same words to the user — the device is unreachable and this
        // resumes when it is back — even though the mechanics beneath differ.
        display_state: match row.state {
            crate::sync::InboundState::Waiting => "waiting_peer".to_string(),
            // Variant C: an `Announced` row whose lane is blocked on the receive gate
            // is WAITING FOR A FREE RECEIVE SLOT (`sync.max_concurrent_receives`) —
            // durably indistinguishable from one that just arrived, so the live
            // parked set is the only thing that can tell the user which it is.
            //
            // "queued" is the shared vocabulary of every "this starts by itself,
            // nothing is wrong" state across the feature: the outbound local queue,
            // variant B's lane-queue ghosts, and this. One word, one meaning, both
            // directions.
            //
            // `Announced` ONLY. The parked set is live and the rows are read a moment
            // apart, so an entry can outlive the park by an instant; gating on the one
            // state a parked row can actually be in means a stale entry is inert
            // rather than a mislabelled running transfer.
            crate::sync::InboundState::Announced if parked_for_slot.contains(&row.package_id) => {
                "queued".to_string()
            }
            other => other.as_str().to_string(),
        },
        stalled_until: None,
        state: row.state,
        generation: row.generation,
        batch_uuid,
        frame_count: row.frame_count,
        byte_size: row.byte_size,
        bytes_done: row.bytes_done,
        created_at: row.created_at,
        display_name: row.display_name.filter(|s| !s.trim().is_empty()),
        device_name: device_names.get(&row.peer).cloned(),
        file_counts: file_counts.get(&row.id).copied().unwrap_or_default(),
        last_error: row.last_error,
        peer_kind,
    }
}

/// The send-side rollup: live in-flight counts + rows from the engine's
/// non-terminal snapshot, plus terminal totals counted from `sync_outbound`.
async fn build_sender_status(
    ctx: &ServiceContext,
    sender: &SyncSenderRuntime,
) -> Result<SyncSenderStatus, ApiError> {
    // Roll up the live snapshot of EVERY started peer engine (per-peer map,
    // sync 2C) under one lock so the in-flight view spans all destinations. The
    // same pass collects each peer's live serve signal (variant A): which package
    // that peer is pulling from us right now, if any. It is read here, under the
    // lock we already hold, because the map is keyed by peer and a row's peer is
    // what associates it with an engine — `dedup_active_rows` below collapses rows
    // by id but every surviving row keeps its `peer`, so the association holds.
    let (started, active_rows, serving_by_peer) = {
        let guard = sender.lock_inner().await;
        let started = !guard.is_empty();
        let mut active_rows: Vec<OutboundRow> = Vec::new();
        let mut serving_by_peer: HashMap<NodeId, i64> = HashMap::new();
        for (peer, s) in guard.iter() {
            active_rows.extend(
                s.engine
                    .status_snapshot()
                    .map_err(|e| ApiError::Internal(format!("sender status snapshot: {e:#}")))?,
            );
            if let Some(active) = s.engine.actively_serving(SERVE_ACTIVITY_FRESHNESS) {
                serving_by_peer.insert(*peer, active);
            }
        }
        (started, active_rows, serving_by_peer)
    };

    // Every engine's `non_terminal()` reads the same peer-unfiltered store, so
    // `active_rows` holds one copy per started engine — dedup by id before
    // counting/summarizing so `queued`/`transferring` reflect distinct packages,
    // not N×, and each package's manifest is read exactly once.
    let rows = dedup_active_rows(active_rows);

    // One DB borrow for everything the summary needs: ONE grouped per-file-counts
    // query for the whole active set (§D5 — never a per-row loop), the device-name
    // cache (one settings read, no hub), and the terminal totals.
    let (file_counts, device_names, confirmed_total, failed_total, cancelled_total) = {
        let db = db(ctx)?;
        let conn = db.conn();
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        (
            outbound_file_counts(&conn, &ids).map_err(|e| ApiError::Internal(format!("{e:#}")))?,
            cached_device_names(&conn),
            count_outbound_state(&conn, "confirmed")?,
            count_outbound_state(&conn, "failed")?,
            count_outbound_state(&conn, "cancelled")?,
        )
    };

    let now = chrono::Utc::now();
    let active: Vec<OutboundSummary> = rows
        .into_iter()
        .map(|row| {
            // A row is "queued at the receiver" iff ITS peer is serving something
            // right now and that something is a different package — the package
            // being served is the one everything else is queued behind, never queued
            // itself.
            let receiver_busy = row_receiver_busy(row.id, serving_by_peer.get(&row.peer).copied());
            outbound_summary(row, now, &device_names, &file_counts, receiver_busy)
        })
        .collect();

    let mut queued = 0u32;
    let mut transferring = 0u32;
    for row in &active {
        match row.state {
            OutboundState::Transferring | OutboundState::Delivered => transferring += 1,
            _ => queued += 1, // Queued / Announced
        }
    }

    Ok(SyncSenderStatus {
        started,
        queued,
        transferring,
        confirmed_total,
        failed_total,
        cancelled_total,
        active,
    })
}

/// The receive-side Active-tab rows: every non-terminal `sync_inbound` row (Task
/// 14), mapped to display summaries. Oldest-first (the store's ordering).
fn active_inbound_summaries(
    ctx: &ServiceContext,
    parked_for_slot: &HashSet<String>,
) -> Result<Vec<InboundSummary>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let rows = inbound_active(&conn).map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    // ONE grouped per-file-counts query for the whole active set (§D5), plus the
    // device-name cache (one settings read, no hub).
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let file_counts =
        inbound_file_counts(&conn, &ids).map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    let device_names = cached_device_names(&conn);
    let device_kinds = cached_device_kind_map(&conn);
    Ok(rows
        .into_iter()
        .map(|row| {
            inbound_summary(
                row,
                &device_names,
                &device_kinds,
                &file_counts,
                parked_for_slot,
            )
        })
        .collect())
}

/// The receive-side GHOST rows (variant B): announces the router handed to a
/// peer's lane that the lane has not started processing yet, mapped for display.
///
/// A device that sends a second batch while the first is still fetching gets no
/// `sync_inbound` row for it until its lane drains — so without this the receiver
/// shows nothing at all for a batch that has, in fact, arrived and is waiting. The
/// live [`InboundControl`](crate::sync::InboundControl) map is the only record of
/// it; no receiver started ⇒ no map ⇒ an empty vec (never an error).
///
/// **Duplicate rule.** A queued entry whose `batch_uuid` already appears among
/// `active` is DROPPED. The sender re-announces on every backoff rung while it
/// waits for an ack, so the batch currently being fetched re-queues on its own
/// lane behind itself — surfacing that would render a ghost next to the live row
/// for the same transfer, and the Transfers UI keys rows on `batchUuid`, so it
/// would be a literal duplicate key. The comparison is on `batch_uuid` alone
/// (not `(peer, batch_uuid)`): that is the identity the UI keys on, and it is
/// sender-minted-uuid unique in practice. In-memory against the rows this same
/// `get_status` already fetched — no extra query.
///
/// Named limit, not a bug: `active` holds only NON-terminal rows, so a re-announce
/// of an already-terminal batch (a declined/failed transfer the sender is
/// retrying) can ghost alongside the terminal row the frontend merges in from
/// [`list_terminal_transfers`]. It resolves itself the moment that lane picks the
/// announce up and resets the row.
fn queued_inbound_summaries(
    ctx: &ServiceContext,
    control: Option<&crate::sync::InboundControl>,
    active: &[InboundSummary],
) -> Result<Vec<QueuedInboundSummary>, ApiError> {
    let Some(control) = control else {
        return Ok(Vec::new());
    };
    let entries = control.queued_announces_snapshot();
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let live: HashSet<&str> = active.iter().map(|r| r.batch_uuid.as_str()).collect();
    let device_names = {
        let db = db(ctx)?;
        let conn = db.conn();
        cached_device_names(&conn)
    };
    Ok(entries
        .into_iter()
        .filter(|e| !live.contains(e.batch_uuid.as_str()))
        .map(|e| {
            let hex = node_id_hex(&e.peer);
            QueuedInboundSummary {
                peer_short: short_id(&hex),
                device_name: device_names.get(&hex).cloned(),
                batch_uuid: e.batch_uuid,
                batch_name: e.queued.batch_name,
                frame_count: e.queued.frame_count,
                byte_size: e.queued.byte_size,
            }
        })
        .collect())
}

/// Transport-reachability health for the status poll (Task 3.3), resolved with
/// NO network I/O and WITHOUT ever binding a node — a status poll must never spin
/// up the transport just to report on it.
///
/// PEEKS at the (possibly-unbound) shared node on [`ServiceContext::iroh_node`]:
/// - **bound** → its own [`SharedIrohNode::transport_health`] (`relay_connected`
///   / `direct_only`); a bound node always has a resolved relay map, so
///   `no_relay_map` can't apply.
/// - **unbound** → distinguish a signed-in device stuck with no relay
///   configuration (`no_relay_map` — worth surfacing, transfers would stall) from
///   a device that simply hasn't started the transport yet (`not_started`). Both
///   branches read only local settings ([`account_signed_in`] + [`cached_relays`],
///   the same cache [`resolve_relay_mode`] would build a node from) — never the hub.
async fn derive_transport_health(ctx: &ServiceContext) -> Result<TransportHealth, ApiError> {
    // Peek only: clone the Arc out if a node is bound, then drop the lock. Never
    // `ensure_iroh_node` here — that would lazily bind a transport on a poll.
    let node = { ctx.iroh_node.lock().await.as_ref().map(Arc::clone) };
    if let Some(node) = node {
        return Ok(node.transport_health());
    }
    if account_signed_in(ctx)? && cached_relays(ctx)?.is_empty() {
        return Ok(TransportHealth::no_relay_map());
    }
    Ok(TransportHealth::not_started())
}

/// Enriched snapshot for the Transfers UI (task M3): pairing summary + send-side
/// rollup + receive-side rollup + transport health (Task 3.3), all resolved
/// without any network I/O so a 10-second UI poll never hits the hub.
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
    // Both queued-ness signals live on the RUNNING receiver's control (None before
    // start ⇒ nothing queued anywhere, never an error).
    let inbound_control = sync.inbound_control().await;
    // Variant C: the rows whose lane is parked on the receive gate. The set and the
    // rows are read a moment apart, so a transfer that crosses the gate in that gap
    // can be labelled one poll late either way — harmless in both directions, since
    // the relabel only ever applies to a row still sitting in `Announced` and the
    // poll repeats every 10 seconds.
    let parked_for_slot = inbound_control
        .as_deref()
        .map(|c| c.parked_for_slot_snapshot())
        .unwrap_or_default();
    let active_inbound = active_inbound_summaries(ctx, &parked_for_slot)?;
    // Variant B: the announces sitting in a peer's lane queue with no row of their
    // own yet, de-duplicated against the active rows just fetched.
    let queued_inbound =
        queued_inbound_summaries(ctx, inbound_control.as_deref(), &active_inbound)?;
    let transport = derive_transport_health(ctx).await?;

    Ok(SyncStatus {
        dev_pairing_enabled,
        transport_started,
        pairing_ticket,
        received_total,
        sender: sender_status,
        receiver: SyncReceiverStatus {
            started: transport_started,
            active: active_inbound,
            queued: queued_inbound,
            received_total,
        },
        transport,
    })
}

/// Default newest-first cap for [`list_terminal_transfers`] when the caller
/// passes no explicit limit — the recent window that covers a session's worth of
/// finished batches.
const TERMINAL_TRANSFERS_DEFAULT_LIMIT: u32 = 100;

/// Hard cap on [`list_terminal_transfers`] regardless of the requested limit: the
/// restart-survival read is a recent-window read, not a pagination surface, so it
/// never returns an unbounded backlog (older batches keep rendering as plain
/// history groups without Resend).
const TERMINAL_TRANSFERS_MAX_LIMIT: u32 = 500;

/// The recently-settled terminal transfers the cheap [`get_status`] poll
/// deliberately omits (tv2 follow-up). `get_status` returns only the
/// `non_terminal()` in-flight rows, so a confirmed/failed/cancelled send — and
/// its inbound twin — would vanish from the Transfers list after a relaunch,
/// taking its durable row id (and thus the **Resend** affordance + Files/Log
/// detail) with it. The frontend re-fetches this on mount and on each
/// `sync-finished` and merges the rows back in.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct TerminalTransfers {
    /// Terminal outbound rows (`confirmed`/`failed`/`cancelled`), newest-first.
    pub sent: Vec<OutboundSummary>,
    /// Terminal inbound rows (`done`/`failed`/`cancelled`), newest-first.
    pub received: Vec<InboundSummary>,
}

/// The recent window of terminal (settled) transfers — outbound
/// (`confirmed`/`failed`/`cancelled`) and inbound (`done`/`failed`/`cancelled`) —
/// read straight from the durable `sync_outbound` / `sync_inbound` tables so they
/// survive a restart (tv2 follow-up). `get_status` intentionally stays a
/// non-terminal, cheap poll; this is the separate, user-action-triggered
/// (mount + `sync-finished`) read the Transfers page merges in to keep a settled
/// row — with its id, Resend affordance and detail — on screen across relaunches.
///
/// Summaries are built through the SAME [`outbound_summary`] / [`inbound_summary`]
/// mappers `get_status` uses, so a terminal row presents identically regardless
/// of which read produced it. Newest-first; `limit` defaults to
/// [`TERMINAL_TRANSFERS_DEFAULT_LIMIT`] and is hard-capped at
/// [`TERMINAL_TRANSFERS_MAX_LIMIT`].
pub fn list_terminal_transfers(
    ctx: &ServiceContext,
    limit: Option<u32>,
) -> Result<TerminalTransfers, ApiError> {
    let limit = limit
        .unwrap_or(TERMINAL_TRANSFERS_DEFAULT_LIMIT)
        .min(TERMINAL_TRANSFERS_MAX_LIMIT);
    let db = db(ctx)?;
    let conn = db.conn();

    let out_rows =
        terminal_outbound(&conn, limit).map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    let in_rows =
        terminal_inbound(&conn, limit).map_err(|e| ApiError::Internal(format!("{e:#}")))?;

    // One DB pass for the shared enrichment: the device-name cache (one settings
    // read, no hub) and ONE grouped per-file-counts query per direction (§D5 —
    // never a per-row loop).
    let device_names = cached_device_names(&conn);
    let device_kinds = cached_device_kind_map(&conn);
    let out_ids: Vec<i64> = out_rows.iter().map(|r| r.id).collect();
    let in_ids: Vec<i64> = in_rows.iter().map(|r| r.id).collect();
    let out_counts =
        outbound_file_counts(&conn, &out_ids).map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    let in_counts =
        inbound_file_counts(&conn, &in_ids).map_err(|e| ApiError::Internal(format!("{e:#}")))?;

    let now = chrono::Utc::now();
    let sent = out_rows
        .into_iter()
        .map(|row| {
            // `resendable` (owner follow-up): Resend would currently succeed only
            // for a terminal failed/cancelled row whose package dir still has its
            // manifest + payload — the exact guard `retry_sync_package` enforces
            // ([`package_has_payload`], reused verbatim, not duplicated). Retention
            // can delete a package's payload after the fact (most commonly after a
            // confirm, but a stale failed/cancelled dir can also be swept), leaving
            // a terminal row whose Resend button would otherwise dead-end in "data
            // missing on disk" — exactly the dead-button bug this flag closes.
            // Computed here (not in the shared mapper / the 10s poll) because it
            // requires an fs-stat of the package dir, paid only on this
            // user-triggered, capped-at-`limit` read.
            let resendable = matches!(row.state, OutboundState::Failed | OutboundState::Cancelled)
                && package_has_payload(Path::new(&row.package_ref));
            // `receiver_busy = false`: this read has no engines in hand (it is a
            // pure DB read, deliberately), and every row it returns is TERMINAL —
            // the display chain never relabels one, so there is nothing to lose.
            let mut summary = outbound_summary(row, now, &device_names, &out_counts, false);
            summary.resendable = resendable;
            summary
        })
        .collect();
    // `parked_for_slot` empty: parking is a property of a live `Announced` row
    // waiting on the receive gate, and every row this read returns is TERMINAL, so
    // no entry could ever match one. Passing the empty set (rather than reaching for
    // the running receiver's control) keeps this a pure DB read.
    let no_parked: HashSet<String> = HashSet::new();
    let received = in_rows
        .into_iter()
        .map(|row| inbound_summary(row, &device_names, &device_kinds, &in_counts, &no_parked))
        .collect();
    Ok(TerminalTransfers { sent, received })
}

/// The transfer history (received + sent), newest first.
pub fn list_history(
    ctx: &ServiceContext,
    query: SyncHistoryQuery,
) -> Result<Vec<HistoryRow>, ApiError> {
    let limit = if query.limit == 0 {
        DEFAULT_HISTORY_LIMIT
    } else {
        query.limit
    };
    let q = HistoryQuery {
        filename: query.filename,
        object: query.object,
        direction: query.direction,
        peer: query.peer,
        project: query.project,
        // The history log is not scoped by batch; per-batch detail is
        // `list_transfer_files`.
        package_id: None,
        limit,
    };
    let db = db(ctx)?;
    let conn = db.conn();
    search_history_rows(&conn, &q).map_err(|e| ApiError::Internal(format!("{e:#}")))
}

/// A generous cap for a single package's per-frame history read — a package holds
/// at most a few thousand frames (each with ≤2 sent rows or ≥1 received row), and
/// the per-batch detail read is a rare user click-through, not the hot poll.
const DETAIL_HISTORY_LIMIT: u32 = 100_000;

/// Basename of a forward-slash manifest `rel_path` (the Transfers UI shows the
/// file, not its in-package sub-path).
fn detail_filename(rel_path: &str) -> String {
    rel_path.rsplit('/').next().unwrap_or(rel_path).to_string()
}

/// The stable per-package batch key recovered from an outbound row's
/// `package_ref`: the dir basename the sender-side history writers
/// ([`crate::sync`]) stamp into `sync_history.package_id`.
fn outbound_package_key(package_ref: &str) -> String {
    Path::new(package_ref)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(package_ref)
        .to_string()
}

/// Per-file detail for one transfer batch — the Transfers UI's expand-a-row
/// view. `direction` selects the outbound (`Sent`) or inbound (`Received`) half;
/// `id` is the durable row id in that half (`sync_outbound.id` /
/// `sync_inbound.id`, from the corresponding summary).
///
/// **Primary source (Transfers Status Model v2 §D4):** the persisted per-file
/// tables (`sync_outbound_files` / `sync_inbound_files`). They exist from enqueue
/// / announce onward, so a batch shows its full structure — `rel_path`, size,
/// per-file `state`, live `bytes_done`, settled `outcome`, and any `error` — at
/// every stage (queued/announced, mid-transfer, terminal), surviving restart.
///
/// **Legacy fallback (a pre-v2 row with no per-file rows):** the old
/// manifest/history join, so old history stays viewable. Sent → the package
/// manifest (names/sizes) joined to this sender's settled history verdicts;
/// received → the received-history rows when terminal, else the staged manifest.
/// The fallback entries carry `state = None` / `error = None` (never tracked
/// per-file for those rows).
pub fn list_transfer_files(
    ctx: &ServiceContext,
    direction: Direction,
    id: i64,
) -> Result<Vec<TransferFileEntry>, ApiError> {
    match direction {
        Direction::Sent => sent_transfer_files(ctx, id),
        Direction::Received => received_transfer_files(ctx, id),
    }
}

/// The [`Direction::Sent`] half of [`list_transfer_files`].
fn sent_transfer_files(ctx: &ServiceContext, id: i64) -> Result<Vec<TransferFileEntry>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let row = outbound_row_by_id(&conn, id)
        .map_err(|e| ApiError::Internal(format!("{e:#}")))?
        .ok_or_else(|| ApiError::NotFound(format!("outbound package {id} not found")))?;

    // Primary source: the per-file rows (present from enqueue onward).
    let file_rows =
        list_outbound_files(&conn, id).map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    if !file_rows.is_empty() {
        return Ok(file_rows
            .into_iter()
            .map(|f| TransferFileEntry {
                name: detail_filename(&f.rel_path),
                rel_path: f.rel_path,
                bytes_total: f.byte_size,
                bytes_done: Some(f.bytes_done),
                state: Some(f.state.as_str().to_string()),
                outcome: f.outcome,
                error: f.error,
            })
            .collect());
    }

    // Legacy fallback: the package manifest joined to this sender's settled
    // per-frame history verdicts (the confirmed/terminal rows carry `finished_at`;
    // the started `sent` rows do not). Newest-first, so the first row per
    // frame_uuid wins (a redelivery's latest verdict).
    let dir = PathBuf::from(&row.package_ref);
    let records = package::read_manifest(&dir)
        .map_err(|e| ApiError::Internal(format!("read manifest {}: {e:#}", dir.display())))?;
    let settled = search_history_rows(
        &conn,
        &HistoryQuery {
            direction: Some(Direction::Sent),
            package_id: Some(outbound_package_key(&row.package_ref)),
            limit: DETAIL_HISTORY_LIMIT,
            ..Default::default()
        },
    )
    .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    let mut outcome_by_frame: HashMap<String, String> = HashMap::new();
    for h in settled.into_iter().filter(|h| h.finished_at.is_some()) {
        outcome_by_frame.entry(h.frame_uuid).or_insert(h.outcome);
    }

    Ok(records
        .iter()
        .map(|r| TransferFileEntry {
            rel_path: r.rel_path.clone(),
            name: detail_filename(&r.rel_path),
            bytes_total: r.byte_size,
            bytes_done: None,
            state: None,
            outcome: outcome_by_frame.get(&r.frame_uuid).cloned(),
            error: None,
        })
        .collect())
}

/// The [`Direction::Received`] half of [`list_transfer_files`].
fn received_transfer_files(
    ctx: &ServiceContext,
    id: i64,
) -> Result<Vec<TransferFileEntry>, ApiError> {
    let (sync_dir, _db_path) = sync_paths(ctx)?;
    let db = db(ctx)?;
    let conn = db.conn();
    let row = get_inbound_by_row_id(&conn, id)
        .map_err(|e| ApiError::Internal(format!("{e:#}")))?
        .ok_or_else(|| ApiError::NotFound(format!("inbound package {id} not found")))?;

    // Primary source: the per-file rows written at announce time (§D4) —
    // available at every stage (announced / fetching / terminal).
    let file_rows =
        list_inbound_files(&conn, id).map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    if !file_rows.is_empty() {
        return Ok(file_rows
            .into_iter()
            .map(|f| TransferFileEntry {
                name: detail_filename(&f.rel_path),
                rel_path: f.rel_path,
                bytes_total: f.byte_size,
                bytes_done: Some(f.bytes_done),
                state: Some(f.state.as_str().to_string()),
                outcome: f.outcome,
                error: f.error,
            })
            .collect());
    }

    // Legacy fallback (a v1/unnamed batch or pre-v2 row with no per-file rows).
    if row.state.is_terminal() {
        // History is the durable record of what landed. Newest-first; keep the
        // first row per frame_uuid (a redelivery's latest verdict). History has no
        // rel_path, so the basename stands in.
        let rows = search_history_rows(
            &conn,
            &HistoryQuery {
                direction: Some(Direction::Received),
                package_id: Some(row.package_id.clone()),
                limit: DETAIL_HISTORY_LIMIT,
                ..Default::default()
            },
        )
        .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        for h in rows {
            if seen.insert(h.frame_uuid.clone()) {
                out.push(TransferFileEntry {
                    rel_path: h.filename.clone(),
                    name: h.filename,
                    bytes_total: h.bytes,
                    bytes_done: Some(h.bytes),
                    state: None,
                    outcome: Some(h.outcome),
                    error: None,
                });
            }
        }
        return Ok(out);
    }

    // Still active: backfill names/sizes from the staged manifest if the fetch has
    // landed it (the receiver stages under `<sync_dir>/staging/<package_id>`), else
    // an honest empty list — the live per-file bars come from `sync-file-progress`.
    let staging = sync_dir.join("staging").join(&row.package_id);
    match package::read_manifest(&staging) {
        Ok(records) => Ok(records
            .iter()
            .map(|r| TransferFileEntry {
                rel_path: r.rel_path.clone(),
                name: detail_filename(&r.rel_path),
                bytes_total: r.byte_size,
                bytes_done: None,
                state: None,
                outcome: None,
                error: None,
            })
            .collect()),
        Err(_) => Ok(Vec::new()),
    }
}

/// One entry in a batch's event journal (Transfers Status Model v2 §D7), for the
/// detail pane's **Log** tab. The presentation slice of a
/// [`SyncEventRow`](crate::sync::SyncEventRow) (its `id` + `direction` are the
/// query scope, not shown).
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct TransferEventEntry {
    /// RFC3339 UTC timestamp of the event.
    pub ts: String,
    /// Short snake_case event kind (`announce_sent`, `dial_failed`,
    /// `retry_scheduled`, `serve_started`, `uploaded`, `ack_received`,
    /// `fetch_started`, `ingested`, `cancelled`, …).
    pub kind: String,
    /// Optional human detail (e.g. a class-tagged reason). `None` for a bare event.
    pub detail: Option<String>,
}

/// The event journal for one transfer batch (Transfers Status Model v2 §D7),
/// newest-first — the detail pane's **Log** tab. `direction` selects the send
/// (`Sent`) or receive (`Received`) half; `id` is the durable row id in that half
/// (`sync_outbound.id` / `sync_inbound.id`), which is the journal's `batch_key`.
/// Fired on detail-pane open (not per-frame), so a plain read is fine.
pub fn list_transfer_events(
    ctx: &ServiceContext,
    direction: Direction,
    id: i64,
) -> Result<Vec<TransferEventEntry>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let rows = list_sync_events(&conn, direction, &id.to_string())
        .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    Ok(rows
        .into_iter()
        .map(|e| TransferEventEntry {
            ts: e.ts,
            kind: e.kind,
            detail: e.detail,
        })
        .collect())
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

/// Map of node-id-hex → device capability (`"athenaeum"` / `"perseus"`) — the
/// capability sibling of [`get_sync_device_names`], for the Transfers UI's
/// per-transfer origin badge (Perseus UI v2, Task 10). Reads the hub's live
/// device list and keys each device by its decoded node id (hex), mapping the
/// device's [`DeviceCapability`](crate::account::DeviceCapability) to its
/// lowercase wire string. Best-effort: a hub that is unreachable or a signed-out
/// device yields an empty map (logged at `debug`, never an error) — the UI simply
/// shows no capability. (The receive-side badge reads the persisted
/// `peer_kind`/`SYNC_PEER_CAPABILITIES` cache; this hub-live map is for the
/// send-side device picker Task 11 also drives.)
pub async fn get_sync_device_capabilities(
    ctx: &ServiceContext,
) -> Result<HashMap<String, String>, ApiError> {
    let devices = match crate::api::account::list_devices(ctx).await {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!(error = %format!("{e:?}"), "device capabilities unavailable; falling back to empty");
            return Ok(HashMap::new());
        }
    };
    let mut map = HashMap::new();
    for d in devices {
        if let Ok(id) = pairing::node_id_from_pubkey_b64(&d.pubkey) {
            map.insert(node_id_hex(&id), d.capability.as_str().to_string());
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

/// A resolved send destination: the peer node id plus its hub-reported endpoint
/// address (finding H1, T7), threaded from [`resolve_dest_node`] through
/// [`enqueue_sync_selection`] into [`ensure_sender_engine`] so the sender dials
/// the peer's REAL relay instead of guessing from our own relay set.
#[derive(Debug, Clone)]
pub struct ResolvedDest {
    /// The destination peer's node id (decoded from its account pubkey).
    pub node: NodeId,
    /// The peer's self-reported endpoint address, when the hub served one
    /// (`None` on an older hub or a device that never reported).
    pub endpoint_addr: Option<crate::account::EndpointAddrReport>,
}

/// Resolve an account device id → its [`NodeId`] (+ reported endpoint address)
/// via the account device list — the send-side counterpart of the receiver's
/// allow-list resolver. Fetches the hub's device list, finds the device with
/// `id == device_id`, and decodes its base64 `pubkey` into a node id. Errors (all
/// [`ApiError::Invalid`], surfaced to the UI) when the device is absent, its
/// pubkey is undecodable, or — per spec §10 — it is a send-only Perseus agent
/// (never a valid destination: a Perseus node has no receiver, so a package sent
/// to it would never land). The device's `endpoint_addr` is carried through so
/// [`ensure_sender_engine`] can dial the peer's real relay (T7).
pub async fn resolve_dest_node(
    ctx: &ServiceContext,
    device_id: &str,
) -> Result<ResolvedDest, ApiError> {
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
    let node = pairing::node_id_from_pubkey_b64(&device.pubkey).map_err(|e| {
        ApiError::Invalid(format!(
            "destination device {device_id} has an invalid pubkey: {e:#}"
        ))
    })?;
    Ok(ResolvedDest {
        node,
        endpoint_addr: device.endpoint_addr.clone(),
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
    ctx: &Arc<ServiceContext>,
    sender: &Arc<SyncSenderRuntime>,
    collab_sender: Arc<SyncSenderRuntime>,
    sync: &SyncRuntime,
    dest: NodeId,
    dest_endpoint_addr: Option<&crate::account::EndpointAddrReport>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<(Arc<SyncEngineHandle>, String), ApiError> {
    let mut guard = sender.lock_inner().await;
    if let Some(started) = guard.get(&dest) {
        return Ok((Arc::clone(&started.engine), started.origin_device.clone()));
    }

    let peer = dest;
    // Relay URLs are still resolved here for the dial hint (a bare
    // account-resolved dest is undialable without one); the shared node resolves
    // the relay MODE once, inside `ensure_iroh_node`.
    let (_relay_mode, relay_urls) = resolve_relay_mode(ctx).await?;
    let (_sync_dir, db_path) = sync_paths(ctx)?;

    // The ONE shared iroh node (C1 fix): the personal sender is its `Out` role
    // handle, sharing the single endpoint + `<sync>/blobs` store with the
    // receiver and the collab sender. Before this, each of these three bound its
    // OWN endpoint from the SAME device key over a separate store (`blobs` /
    // `blobs_out` / `blobs_collab`); a relay admits one connection per node id,
    // so they evicted each other and inbound datagrams reached only whichever
    // endpoint held the relay slot. One endpoint removes that self-collision;
    // role-prefixed blob tags (Д3) keep the roles from clobbering each other's
    // tags on the shared store.
    let node = ensure_iroh_node(ctx).await?;
    // Bound relay-map staleness (T8, H2): ensure the node's hourly relay refresh
    // loop is running. Idempotent — the receiver's autostart usually started it
    // first; this covers a sender-first bind.
    start_node_relay_refresh(Arc::clone(ctx), &node);
    // Wake event → kick pending packages (T6): install here too so a sender-first
    // bind (send before the receiver autostarted) still wakes both sender maps on
    // a relay reconnect / relay-map change.
    install_node_wake_hook(ctx, &node, Arc::clone(sender), Arc::clone(&collab_sender));
    // Peer reachability (D1) is deliberately NOT installed here. This path holds
    // `sync` as a borrow, and the hook needs a `'static` handle — but more to the
    // point it would be dead wiring: a process that reached a sender-first bind
    // without `autostart_if_enabled` having run is one that is not signed in
    // (that is the autostart gate), and an unsigned device has no cached
    // authorized-peer set, so `handle_peer_presence`'s first gate would refuse
    // every beacon anyway. The two sites that DO have the account are the two that
    // install it.
    // Periodic authorized-peers refresh (task 7): install here too so a
    // sender-first process still runs the hourly hub re-pull (idempotent — no-ops
    // if a receiver-start site already installed it, which it always has for a
    // signed-in node that autostarted).
    ensure_peers_refresh_task(ctx, sync).await;
    let transport: Arc<dyn SharingTransport> = node.handle(Role::Out);
    let origin_device = node_id_hex(&node.node_id());

    // The destination is an account-resolved bare node id (from
    // `resolve_dest_node`). The node binds with `presets::Minimal` (no discovery
    // services), so without a dial hint `announce` fails instantly with "No
    // addressing information available". Prefer the peer's OWN hub-reported
    // address (its real home relay + direct addrs — same account, so direct is
    // allowed) via `peer_dial_addr`, falling back to our own resolved relay set
    // when the peer never reported (T7 / finding H1). `cross_account = false`:
    // both devices are in this account.
    let peer_addr = pairing::peer_dial_addr(peer, dest_endpoint_addr, &relay_urls, false)
        .map_err(|e| ApiError::Internal(format!("construct peer address: {e:#}")))?;
    node.add_peer(peer_addr);

    let store = Arc::new(
        CatalogSyncStore::open(&db_path)
            .map_err(|e| ApiError::Internal(format!("open catalog sync store: {e:#}")))?,
    );
    let engine = Arc::new(SyncEngine::spawn_with_emitter_and_refresher(
        store as Arc<dyn SyncStore>,
        transport,
        peer,
        emitter,
        // T8: on a timed-out retry, re-resolve this peer's current address so a
        // relay-map change or the peer moving relays doesn't strand every retry.
        Some(sender_addr_refresher(Arc::clone(ctx))),
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
    /// The resolved human batch name (T3). `None` when nothing was eligible.
    display_name: Option<String>,
    /// Per-payload `(rel_path, byte_size, frame_uuid)` for the `sync_outbound_files`
    /// rows written after the row id is minted. One entry per manifest record.
    files: Vec<(String, u64, String)>,
}

/// What a package build starts from: the entries to write plus the caller's
/// already-known ineligible frames and reporting context.
pub(crate) struct SelectionInput {
    pub(crate) entries: Vec<PayloadEntry>,
    pub(crate) ineligible: Vec<IneligibleFrame>,
    /// Common ancestor of the selection's files (browser sends) — feeds the
    /// auto batch name. `None` for frame-set sends.
    pub(crate) ancestor: Option<PathBuf>,
    /// Frames the caller was asked for (the `M` in `N of M`).
    pub(crate) total: usize,
}

/// The package `rel_path` layout chosen for one send (T3, spec §D2).
enum RelPathLayout {
    /// Object send: `frame_id → WBPP rel_dir` (forward-slash, frame-set name NOT
    /// included — the receiver adds its own `<batch_name>` landing level).
    Wbpp(HashMap<i64, String>),
    /// Browser send: each file's path taken relative to this common ancestor dir.
    /// `None` → no usable common ancestor (flat basename fallback).
    SourceRelative(Option<PathBuf>),
}

/// The deepest directory that contains every path in `paths` (each path is a
/// FILE, so its parent is the candidate). Returns `None` when the selection is
/// empty, any path has no parent, or the common prefix collapses to a bare
/// filesystem root (no `Normal` component) — the "flat fallback" trigger (§D2).
fn common_ancestor(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut prefix: PathBuf = paths.first()?.parent()?.to_path_buf();
    for p in paths.iter().skip(1) {
        let parent = p.parent()?;
        prefix = longest_common_dir(&prefix, parent);
        if prefix.as_os_str().is_empty() {
            return None;
        }
    }
    // A prefix with no Normal component is `/` (or a bare prefix) → treat as none.
    if !prefix
        .components()
        .any(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(prefix)
}

/// The longest shared leading run of directory components of `a` and `b`.
fn longest_common_dir(a: &Path, b: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for (ca, cb) in a.components().zip(b.components()) {
        if ca == cb {
            result.push(ca.as_os_str());
        } else {
            break;
        }
    }
    result
}

/// Forward-slash directory of `path` relative to `ancestor` (the dirs BETWEEN the
/// ancestor and the file; empty when the file sits directly in the ancestor).
/// `None` when `path` is not under `ancestor`.
fn source_rel_dir(path: &str, ancestor: &Path) -> Option<String> {
    let rel = Path::new(path).strip_prefix(ancestor).ok()?;
    let parent = rel.parent()?;
    Some(
        parent
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Split a filename into `(stem, extension?)` on the LAST dot (`a.b.fits` →
/// `("a.b", Some("fits"))`, `noext` → `("noext", None)`).
fn split_stem_ext(filename: &str) -> (String, Option<String>) {
    let p = Path::new(filename);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| filename.to_string());
    let ext = p.extension().map(|s| s.to_string_lossy().into_owned());
    (stem, ext)
}

/// A collision-free filename WITHIN one target directory: returns `filename`
/// unchanged on first use, else suffixes before the extension (`name_2.fits`,
/// `name_3.fits`, …). Collision scope is per-directory (§D2 — never flatten dirs),
/// so `used` is keyed on the directory.
fn dedup_in_dir(dir: &str, filename: &str, used: &mut HashMap<String, HashSet<String>>) -> String {
    let set = used.entry(dir.to_string()).or_default();
    if set.insert(filename.to_string()) {
        return filename.to_string();
    }
    let (stem, ext) = split_stem_ext(filename);
    let mut n = 2u32;
    loop {
        let candidate = match &ext {
            Some(e) => format!("{stem}_{n}.{e}"),
            None => format!("{stem}_{n}"),
        };
        if set.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// Resolve one payload's package `rel_path` under `layout`, suffixing on a
/// per-directory filename collision. Always forward-slash and validate-safe.
fn assign_rel_path(
    layout: &RelPathLayout,
    frame_id: i64,
    file: &crate::models::File,
    used_by_dir: &mut HashMap<String, HashSet<String>>,
) -> String {
    let base = if file.filename.trim().is_empty() {
        format!("frame_{frame_id}.fits")
    } else {
        file.filename.clone()
    };
    let dir = match layout {
        // WBPP: a frame not present in the placement map (e.g. selected but not a
        // member of this frame set) falls back to a flat basename.
        RelPathLayout::Wbpp(map) => map.get(&frame_id).cloned().unwrap_or_default(),
        RelPathLayout::SourceRelative(Some(ancestor)) => {
            source_rel_dir(&file.path, ancestor).unwrap_or_default()
        }
        RelPathLayout::SourceRelative(None) => String::new(),
    };
    let name = dedup_in_dir(&dir, &base, used_by_dir);
    if dir.is_empty() {
        name
    } else {
        format!("{dir}/{name}")
    }
}

/// The frame set's name for the auto-batch-name (§D1). `Some(raw name)` when the
/// row exists (a NULL/blank name yields `"Frame Set {id}"`, matching the export
/// collector); `None` when there is no such row.
fn frame_set_name(conn: &rusqlite::Connection, frame_set_id: i64) -> Option<String> {
    match conn.query_row(
        "SELECT name FROM frames_set WHERE id = ?1",
        [frame_set_id],
        |row| row.get::<_, Option<String>>(0),
    ) {
        Ok(Some(name)) if !name.trim().is_empty() => Some(name),
        Ok(_) => Some(format!("Frame Set {frame_set_id}")),
        Err(_) => None,
    }
}

/// Local wall-clock `YYYY-MM-DD HH:MM` for the loose-files auto-name (§D1).
fn local_now_short() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()
}

/// Resolve the batch's human `display_name` (§D1). A user-provided non-blank name
/// is kept VERBATIM (raw human string — sanitizing happens only for the receiver
/// landing path, later). Otherwise: frame-set name → common-ancestor basename →
/// `"{N} files — {YYYY-MM-DD HH:MM}"`.
fn resolve_batch_name(
    user: Option<&str>,
    conn: &rusqlite::Connection,
    frame_set_id: Option<i64>,
    ancestor: Option<&Path>,
    file_count: usize,
) -> String {
    if let Some(n) = user {
        if !n.trim().is_empty() {
            return n.to_string();
        }
    }
    if let Some(fsid) = frame_set_id {
        if let Some(name) = frame_set_name(conn, fsid) {
            return name;
        }
    }
    if let Some(dir) = ancestor {
        if let Some(base) = dir.file_name().and_then(|s| s.to_str()) {
            if !base.is_empty() {
                return base.to_string();
            }
        }
    }
    format!("{file_count} files — {}", local_now_short())
}

/// A collision-free package `rel_path` for a payload. Uses the source filename;
/// on a duplicate basename within one package it disambiguates with the frame id
/// (and, in the pathological case, a uuid) so no two payloads overwrite.
/// Sole remaining caller is the render-gated `api::collab` publish path (the
/// send path moved to structured WBPP rel_paths), so gate it the same way to
/// keep the headless (`default-features = false`) build warning-free.
#[cfg(feature = "render")]
pub(crate) fn unique_rel_path(filename: &str, frame_id: i64, used: &mut HashSet<String>) -> String {
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

/// Write the full hashes a manifest build already computed into
/// `files.strong_hash`, one transaction for the batch. Best-effort by design:
/// the package is the product, the banked hash is a by-product — an error is
/// logged and the send goes on, and a row that vanished meanwhile (0 rows
/// updated) is simply not counted.
fn bank_manifest_hashes(conn: &rusqlite::Connection, bank: &[(i64, String)]) {
    if bank.is_empty() {
        return;
    }
    let write = || -> rusqlite::Result<usize> {
        let tx = conn.unchecked_transaction()?;
        let mut n = 0usize;
        for (file_id, hash) in bank {
            n += crate::db::bank_strong_hash(&tx, *file_id, hash)?;
        }
        tx.commit()?;
        Ok(n)
    };
    match write() {
        Ok(n) => tracing::debug!(count = n, "manifest build: strong_hash banked"),
        Err(e) => tracing::error!(error = %e, count = bank.len(), "manifest build: strong_hash bank failed"),
    }
}

/// Resolve a frame-id selection into payload entries — the catalog-lookup half
/// of a send: which requested ids are real frames, where each one's file sits,
/// and the package `rel_path` it gets under the chosen layout (§D2). Ids with no
/// catalog row come back as ineligible here; the missing/unreadable-file half of
/// eligibility belongs to [`build_selection_package`], which is what touches disk.
fn selection_entries(
    conn: &rusqlite::Connection,
    frame_ids: &[i64],
    frame_set_id: Option<i64>,
) -> Result<SelectionInput, ApiError> {
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

    // Common ancestor of the selection's files — drives both the source-relative
    // rel_path layout and the common-ancestor auto-name. Computed over every row
    // that resolves to a catalog frame (a to-be-ineligible missing file only makes
    // the ancestor shallower, never wrong).
    let candidate_paths: Vec<PathBuf> = rows
        .iter()
        .filter(|(_, _, frame)| frame.id.is_some())
        .map(|(_, file, _)| PathBuf::from(&file.path))
        .collect();
    let ancestor = common_ancestor(&candidate_paths);

    // Choose the rel_path layout (§D2): object send → WBPP hierarchy; browser send
    // → source-relative. A WBPP build failure degrades to source-relative (never
    // fails the send) with a warn — the honest-fallback rule.
    let layout = match frame_set_id {
        Some(fsid) => match crate::export::collect_export_data(conn, fsid) {
            Ok(data) => {
                let map: HashMap<i64, String> =
                    crate::export::file_organizer::compute_wbpp_placements(&data)
                        .into_iter()
                        .map(|p| (p.frame_id, p.rel_dir))
                        .collect();
                RelPathLayout::Wbpp(map)
            }
            Err(e) => {
                tracing::warn!(
                    frame_set_id = fsid,
                    error = %format!("{e:#}"),
                    "WBPP layout unavailable; using source-relative send layout"
                );
                RelPathLayout::SourceRelative(ancestor.clone())
            }
        },
        None => RelPathLayout::SourceRelative(ancestor.clone()),
    };

    // Per-directory used-filename sets for collision suffixing (§D2).
    let mut used_by_dir: HashMap<String, HashSet<String>> = HashMap::new();
    let mut resolved: HashSet<i64> = HashSet::new();
    let mut entries = Vec::with_capacity(rows.len());
    for (_file_id, file, frame) in &rows {
        let Some(frame_id) = frame.id else { continue };
        resolved.insert(frame_id);
        // A hand-picked master is labeled honestly, which is also what keeps it
        // out of the retention linkage in the builder below (§D5). The test is
        // the CATALOG's own master signal, not the header alone: the scanner
        // sets `frames.is_master` from `imagetyp.is_master() || filename_is_master`
        // (`fits_parser/mod.rs`), so a `master_dark_*.fits` whose IMAGETYP still
        // reads `Dark` must not fall through to `RawFrame` and become
        // retention-reclaimable.
        let kind = if frame.is_master || frame.imagetyp.as_ref().is_some_and(|t| t.is_master()) {
            PayloadKind::Master
        } else {
            PayloadKind::RawFrame
        };
        entries.push(PayloadEntry {
            frame_id,
            source_path: PathBuf::from(&file.path),
            rel_path: assign_rel_path(&layout, frame_id, file, &mut used_by_dir),
            kind,
        });
    }

    // Requested ids that never resolved to a catalog row at all.
    let ineligible = requested
        .iter()
        .filter(|id| !resolved.contains(id))
        .map(|id| IneligibleFrame {
            frame_id: *id,
            reason: "frame not found in catalog".to_string(),
        })
        .collect();

    Ok(SelectionInput {
        entries,
        ineligible,
        ancestor,
        total,
    })
}

/// Build ONE package from exactly the payload entries in `input`. INELIGIBLE
/// entries — whose frame is not (or no longer) in the catalog, or whose file is
/// missing/unreadable on disk — are collected and returned, never silently
/// dropped (task M2). The manifest mirrors what Perseus builds (serialized
/// `models::Frame` as `frame_meta` + the analysis summary when present) so the
/// primary ingests app- and Perseus-sourced frames identically.
///
/// Per-kind rules (spec 2026-08-28 §3 step 5 / §D5):
/// - [`PayloadKind::RawFrame`] — the frame's own snapshot and uuid, `strong_hash`
///   banked, and the ONLY kind that gets a `sync_sources` retention linkage row.
/// - [`PayloadKind::Master`] — the same manifest as a raw frame, but no linkage:
///   a master is shared by construction, so retention must never reclaim it out
///   from under its other consumers.
/// - [`PayloadKind::CalibratedLight`] — an artifact that is not in the catalog at
///   all: it carries the SOURCE light's `frame_meta` under its own fresh uuid,
///   with no analysis, no hash banking (the catalog row describes the source
///   file, not this one) and no linkage.
fn build_selection_package(
    conn: &rusqlite::Connection,
    origin_device: &str,
    packages_dir: &Path,
    input: SelectionInput,
    batch_name: Option<&str>,
    frame_set_id: Option<i64>,
) -> Result<BuiltSelection, ApiError> {
    // Load each referenced frame's snapshot once, indexed by frame id rather than
    // walked row-by-row: two entries may share one frame (a calibrated artifact
    // and its source light both point at the light's catalog row).
    let mut seen_frame = HashSet::new();
    let frame_ids: Vec<i64> = input
        .entries
        .iter()
        .map(|e| e.frame_id)
        .filter(|id| seen_frame.insert(*id))
        .collect();
    let rows = crate::db::get_frames_with_files_by_ids(conn, &frame_ids)
        .map_err(|e| ApiError::Internal(format!("resolve frames for selection: {e:#}")))?;
    let by_frame: HashMap<i64, (i64, &crate::models::File, &crate::models::Frame)> = rows
        .iter()
        .filter_map(|(file_id, file, frame)| Some((frame.id?, (*file_id, file, frame))))
        .collect();
    let analyses = crate::db::analysis::get_frame_analyses_by_ids(conn, &frame_ids)
        .map_err(|e| ApiError::Internal(format!("load analysis summaries: {e:#}")))?;
    let analysis_by_frame: HashMap<i64, &crate::models::FrameAnalysis> =
        analyses.iter().map(|a| (a.frame_id, a)).collect();

    // The caller's own ineligible frames (ids that never resolved) come first;
    // the per-file failures below append to the same list.
    let mut ineligible: Vec<IneligibleFrame> = input.ineligible;
    let mut eligible: Vec<i64> = Vec::new();
    let mut records: Vec<(PathBuf, ManifestRecord)> = Vec::new();
    // Per-directory used-filename sets for collision suffixing (§D2). The caller
    // owns the directory; the filename is deduped within it here so no two
    // entries can overwrite each other inside the package.
    let mut used_by_dir: HashMap<String, HashSet<String>> = HashMap::new();
    // Per-eligible source linkage recorded into `sync_sources` after the package
    // is written: (catalog file_id, absolute path, size, mtime_ms). This is what
    // retention (task M4) later joins on to resolve a confirmed package back to
    // the disk files it may reclaim, with the recorded stat as the TOCTOU guard.
    let mut source_links: Vec<(i64, String, u64, i64)> = Vec::new();
    // The manifest hashes every source file in full; that read is banked as
    // `files.strong_hash` after the loop for every row the disk still vouches
    // for — `disk_matches_row`, the contract shared with the master-hash pass
    // and deep verify — so the Master duplicate key and a later verify get the
    // digest without reading the file again. Collected here, written in one
    // transaction below; a bank failure never fails the send.
    let mut bank: Vec<(i64, String)> = Vec::new();

    for entry in &input.entries {
        let Some((file_id, file, frame)) = by_frame.get(&entry.frame_id).copied() else {
            ineligible.push(IneligibleFrame {
                frame_id: entry.frame_id,
                reason: "frame not found in catalog".to_string(),
            });
            continue;
        };

        let path = entry.source_path.as_path();
        if !path.exists() {
            ineligible.push(IneligibleFrame {
                frame_id: entry.frame_id,
                reason: "file missing on disk".to_string(),
            });
            continue;
        }
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                ineligible.push(IneligibleFrame {
                    frame_id: entry.frame_id,
                    reason: format!("cannot stat file: {e}"),
                });
                continue;
            }
        };
        let byte_size = meta.len();
        let mtime_ms = crate::api::retention::mtime_millis(meta.modified().ok());
        let xxh3 = match package::xxh3_full_file(path) {
            Ok(h) => h,
            Err(e) => {
                ineligible.push(IneligibleFrame {
                    frame_id: entry.frame_id,
                    reason: format!("cannot read file: {e:#}"),
                });
                continue;
            }
        };
        // A calibrated artifact is NOT the file the catalog row describes, so
        // neither its hash nor its analysis may be attributed to that row.
        let is_catalog_file = entry.kind != PayloadKind::CalibratedLight;
        if is_catalog_file
            && crate::duplicates::backfill::disk_matches_row(
                path,
                file.size,
                &file.modified_at.to_rfc3339(),
            )
        {
            bank.push((file_id, xxh3.clone()));
        }
        let frame_meta = match serde_json::to_value(frame) {
            Ok(v) => v,
            Err(e) => {
                ineligible.push(IneligibleFrame {
                    frame_id: entry.frame_id,
                    reason: format!("serialize frame_meta: {e}"),
                });
                continue;
            }
        };
        let analysis = if is_catalog_file {
            analysis_by_frame
                .get(&entry.frame_id)
                .and_then(|a| match serde_json::to_value(a) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::warn!(frame_id = entry.frame_id, error = %e, "sync selection: analysis serialize failed; omitting");
                        None
                    }
                })
        } else {
            None
        };

        // Identity anchor: the catalog frame uuid (the receiver dedups on it).
        // A calibrated artifact is a distinct file from its source light, so it
        // gets its own identity — reusing the light's uuid would make the
        // receiver treat the two as the same frame.
        let frame_uuid = match entry.kind {
            PayloadKind::CalibratedLight => uuid::Uuid::new_v4().to_string(),
            _ => frame
                .uuid
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        };
        // The caller assigned the directory; only the filename is deduped here.
        let (dir, base) = match entry.rel_path.rsplit_once('/') {
            Some((d, b)) => (d.to_string(), b.to_string()),
            None => (String::new(), entry.rel_path.clone()),
        };
        let name = dedup_in_dir(&dir, &base, &mut used_by_dir);
        let rel_path = if dir.is_empty() {
            name
        } else {
            format!("{dir}/{name}")
        };

        records.push((
            path.to_path_buf(),
            ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: frame_uuid.clone(),
                origin_catalog_uuid: frame_uuid,
                origin_device: origin_device.to_string(),
                payload_kind: entry.kind.clone(),
                rel_path,
                byte_size,
                xxh3,
                frame_meta,
                analysis,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
                project: None,
            },
        ));
        eligible.push(entry.frame_id);
        // Retention linkage for raw frames ONLY (§D5): a master is shared by
        // construction and a calibrated artifact is a rebuildable by-product, so
        // neither may be swept as "the source this package delivered".
        if entry.kind == PayloadKind::RawFrame {
            source_links.push((file_id, file.path.clone(), byte_size, mtime_ms));
        }
    }

    bank_manifest_hashes(conn, &bank);

    if records.is_empty() {
        return Ok(BuiltSelection {
            pkg_dir: None,
            eligible,
            ineligible,
            total: input.total,
            display_name: None,
            files: Vec::new(),
        });
    }

    // Resolve the batch's human name (§D1) and snapshot the per-file rows for the
    // `sync_outbound_files` write, before `records` is moved into the writer.
    let display_name = resolve_batch_name(
        batch_name,
        conn,
        frame_set_id,
        input.ancestor.as_deref(),
        records.len(),
    );
    let files: Vec<(String, u64, String)> = records
        .iter()
        .map(|(_, r)| (r.rel_path.clone(), r.byte_size, r.frame_uuid.clone()))
        .collect();

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

    Ok(BuiltSelection {
        pkg_dir: Some(pkg_dir),
        eligible,
        ineligible,
        total: input.total,
        display_name: Some(display_name),
        files,
    })
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
    batch_name: Option<&str>,
    frame_set_id: Option<i64>,
) -> Result<EnqueueSelectionResult, ApiError> {
    let built = {
        let db = db(ctx)?;
        let conn = db.conn();
        let input = selection_entries(&conn, frame_ids, frame_set_id)?;
        build_selection_package(
            &conn,
            origin_device,
            packages_dir,
            input,
            batch_name,
            frame_set_id,
        )?
    };
    if let Some(dir) = &built.pkg_dir {
        // §D1/§D4: the batch name + per-file `pending` rows travel WITH the enqueue
        // now — the engine's `store.enqueue` writes them in the same transaction as
        // the row, so the first announce can never read a nameless / file-less row
        // (the T3 enqueue→announce race is gone; no post-hoc write, no self-heal).
        let files: Vec<crate::sharing::types::AnnounceFileEntry> = built
            .files
            .iter()
            .map(
                |(rel_path, byte_size, frame_uuid)| crate::sharing::types::AnnounceFileEntry {
                    rel_path: rel_path.clone(),
                    byte_size: *byte_size,
                    frame_uuid: frame_uuid.clone(),
                },
            )
            .collect();
        // Desktop senders are out of the mirror-hierarchy v1 scope: a selection
        // package is a curated batch, so it lands in the per-transfer batch folder.
        engine
            .enqueue_package(dir, built.display_name.clone(), files, PackageLayout::Batch)
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
    ctx: &Arc<ServiceContext>,
    sender: &Arc<SyncSenderRuntime>,
    collab_sender: Arc<SyncSenderRuntime>,
    sync: &SyncRuntime,
    dest: ResolvedDest,
    frame_ids: Vec<i64>,
    batch_name: Option<String>,
    frame_set_id: Option<i64>,
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
    let (engine, origin_device) = ensure_sender_engine(
        ctx,
        sender,
        collab_sender,
        sync,
        dest.node,
        dest.endpoint_addr.as_ref(),
        emitter,
    )
    .await?;
    let packages_dir = sender_packages_dir(ctx)?;
    let result = build_and_enqueue_selection(
        ctx,
        &engine,
        &origin_device,
        &packages_dir,
        &frame_ids,
        batch_name.as_deref(),
        frame_set_id,
    )
    .await?;
    tracing::info!(
        enqueued = result.enqueued_count,
        total = result.total_count,
        ineligible = result.ineligible.len(),
        "sync selection enqueued"
    );
    Ok(result)
}

// ── Retry / send-now / cancel command surface (Task 8) ───────────────────────

/// Whether `dir` still holds a package's manifest AND at least one payload file —
/// the retry precondition, ported from Perseus's `api_retry`. A confirmed package
/// is manifest-only after payload cleanup (so this is `false` for it), and a
/// vanished dir is `false`: re-announcing a manifest-only dir can never deliver,
/// so retry rejects it up front.
fn package_has_payload(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut has_manifest = false;
    let mut has_payload = false;
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_file() {
            continue;
        }
        if entry.file_name() == std::ffi::OsStr::new(package::MANIFEST_FILENAME) {
            has_manifest = true;
        } else {
            has_payload = true;
        }
    }
    has_manifest && has_payload
}

/// Resolve the started sender engine that owns the pending (non-terminal) row
/// `id`, for the send-now / cancel command surfaces. The per-peer engines share
/// ONE catalog store, so any started engine's non-terminal snapshot lists every
/// in-flight row (peer-unfiltered); we read the row off the first started engine
/// to learn its `peer`, then route to THAT peer's engine — the only one that
/// actually holds the in-memory pending slot `kick` / `cancel` act on.
///
/// `Invalid("package is not active")` when no started engine's snapshot carries
/// `id`: it is already terminal (a retry candidate, not send-now/cancel),
/// unknown, or its peer has no started engine in this process.
async fn active_engine_for_row(
    sender: &Arc<SyncSenderRuntime>,
    id: i64,
) -> Result<Arc<SyncEngineHandle>, ApiError> {
    for peer in sender.started_peers().await {
        let Some((engine, _)) = sender.current_for(&peer).await else {
            continue;
        };
        let snapshot = engine
            .status_snapshot()
            .map_err(|e| ApiError::Internal(format!("sender status snapshot: {e:#}")))?;
        if let Some(row) = snapshot.into_iter().find(|r| r.id == id) {
            return sender
                .current_for(&row.peer)
                .await
                .map(|(eng, _)| eng)
                .ok_or_else(|| ApiError::Invalid("package is not active".into()));
        }
    }
    Err(ApiError::Invalid("package is not active".into()))
}

/// Resend a terminal outbound transfer as a **reset of the SAME row** (Transfers
/// Batch Model §D1/§D3) — the transfers-not-attempts model. No new `sync_outbound`
/// row is minted: the failed/cancelled row is reset in place (`attempts += 1`, a
/// fresh per-attempt `wire_package_id`, every `sync_outbound_files` row back to
/// `pending`, cleared error/retry/confirm fields) and re-driven on its own engine.
/// It returns the SAME id.
///
/// Guards are unchanged from the old row-minting retry: the row must be terminal
/// (`Failed` / `Cancelled` — a confirmed row is manifest-only after cleanup, a live
/// row is send-now territory), and its payload must still be on disk (`resendable`
/// gates the button on exactly this). A fresh wire id is what makes replay-safe: a
/// cancelled attempt's receipts are keyed by its OWN wire id, so they can never
/// short-circuit the new attempt (spec §D1; verified at the transport in B0-R3).
///
/// The re-drive MUST run on the engine whose peer matches `row.peer` — the row is
/// stamped with that peer. A started engine is reused and told to re-drive the
/// reset row ([`SyncEngineHandle::resend`]); otherwise one is built lazily via
/// [`ensure_sender_engine`] (a restart clears the in-memory engine map while
/// terminal rows persist), whose crash-resume enumeration re-drives the now-`queued`
/// row on its own — the explicit `resend` command is idempotent with that.
pub async fn resend_transfer(
    ctx: &Arc<ServiceContext>,
    sender: &Arc<SyncSenderRuntime>,
    collab_sender: Arc<SyncSenderRuntime>,
    sync: &SyncRuntime,
    id: i64,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<i64, ApiError> {
    let (_sync_dir, db_path) = sync_paths(ctx)?;
    // Read the row from the shared catalog store (an engine may not be started for
    // its peer yet — e.g. after a restart).
    let row = {
        let store = CatalogSyncStore::open(&db_path)
            .map_err(|e| ApiError::Internal(format!("open catalog sync store: {e:#}")))?;
        store
            .get_outbound(id)
            .map_err(|e| ApiError::Internal(format!("read outbound row {id}: {e:#}")))?
            .ok_or_else(|| ApiError::Invalid(format!("unknown package id {id}")))?
    };
    // Only a terminal failed/cancelled row is resendable (a confirmed row is
    // manifest-only after cleanup; a live row is send-now territory, not resend).
    if !matches!(row.state, OutboundState::Failed | OutboundState::Cancelled) {
        return Err(ApiError::Invalid(format!(
            "package {id} is {} — only failed or cancelled packages can be resent",
            row.state.as_str()
        )));
    }
    // The payload must still be on disk — re-announcing a manifest-only dir can
    // never deliver.
    let dir = PathBuf::from(&row.package_ref);
    if !package_has_payload(&dir) {
        return Err(ApiError::Invalid(format!(
            "package {id} data is missing on disk; cannot resend"
        )));
    }
    // Decline exception (Task D): a transfer the RECEIVER declined is final per its
    // `batch_uuid` — re-announcing the same batch just bounces (all-cancelled
    // re-ack). A deliberate sender re-ask therefore mints a NEW transfer with a
    // fresh batch identity (new dir basename ⇒ new wire `batch_uuid`), leaving the
    // declined row as history. Keyed on the exact `last_error` the all-cancelled ack
    // stamps (single Rust source). Every other terminal (sender-cancel, failed)
    // resets the SAME row in place below.
    if row.state == OutboundState::Cancelled
        && row.last_error.as_deref() == Some(CANCELLED_BY_RECEIVER_DETAIL)
    {
        return resend_declined_as_new_transfer(ctx, sender, collab_sender, sync, &row, emitter)
            .await;
    }
    // §D1: mint a fresh per-attempt wire id and reset the SAME row in place (the B2
    // bridge — attempts++, files → pending, error/retry/confirm cleared). The row
    // is terminal here (NOT in the engine's `pending` map), so the next drive picks
    // up the new wire id fresh; nothing is cached across the reset.
    let new_wire_id = uuid::Uuid::new_v4().to_string();
    {
        let store = CatalogSyncStore::open(&db_path)
            .map_err(|e| ApiError::Internal(format!("open catalog sync store: {e:#}")))?;
        store
            .reset_outbound_for_resend(id, &new_wire_id)
            .map_err(|e| ApiError::Internal(format!("reset outbound {id} for resend: {e:#}")))?;
        // Journal the resend (§D3/§D7). Best-effort — a log write must never fail
        // the resend; ordered here after the row's own `cancelled`/`revoke_sent`
        // (already written by the engine's terminal transition) and before the new
        // attempt's `announce_sent`/`confirmed`.
        if let Err(e) = store.append_sync_event(Direction::Sent, &id.to_string(), "resend", None) {
            tracing::warn!(package_id = id, error = %format!("{e:#}"), "append resend sync_event failed");
        }
    }
    // Re-drive on row.peer's engine — reuse a started one or build it lazily.
    let engine = match sender.current_for(&row.peer).await {
        Some((engine, _)) => engine,
        None => {
            let (engine, _origin) =
                ensure_sender_engine(ctx, sender, collab_sender, sync, row.peer, None, emitter)
                    .await?;
            engine
        }
    };
    engine
        .resend(id)
        .await
        .map_err(|e| ApiError::Internal(format!("re-drive resend {id}: {e:#}")))?;
    tracing::info!(id, peer = %node_id_hex(&row.peer), "sync transfer resent");
    Ok(id)
}

/// Resend a **receiver-declined** transfer as a brand-NEW transfer (Task D) — the
/// deliberate-re-ask path. A decline is final per the transfer's `batch_uuid`, so
/// re-announcing the same batch only bounces (all-cancelled re-ack). Instead we
/// give the payload a fresh batch identity and enqueue it as a new row:
///
/// 1. **Sole-owner guard** — refuse if any OTHER outbound row shares this
///    `package_ref` (belt-and-suspenders; desktop package dirs are single-owner by
///    construction, but a shared dir must never be renamed out from under a sibling).
/// 2. **Resolve the peer's engine FIRST, then rename** the payload dir to a fresh
///    `uuid` basename → the announce derives `batch_uuid` from the dir basename at
///    attempt time, so the receiver keys ONE brand-new inbound row (its old
///    declined row stays declined; receiver code is untouched). Engine-before-
///    rename ordering (review fix): engine construction is network/hub-dependent —
///    failing BEFORE any side effect leaves the old row's Resend fully live,
///    whereas the only failure after the rename is the enqueue itself, which has a
///    rename-back. The old row keeps its `package_ref` string (history/delete
///    joins intact) and its Resend affordance recomputes false (payload dir gone).
/// 3. **Re-key** the `sync_sources` retention linkage onto the new dir (warn-and-
///    continue — a failure only means the new transfer's sources aren't reclaimed,
///    the safe direction).
/// 4. **Clone** the manifest (`list_outbound_files`) + `display_name`.
/// 5. **Enqueue** on `row.peer`'s engine — the WORKER inserts the new row (no API
///    pre-insert). On enqueue error, best-effort rename the dir back (and re-key the
///    sources back) so the old row's Resend stays live, then surface `Internal`.
///
///    Crash window (verified vs `dir_age` — recursive max mtime; payload mtimes stay
///    OLD through a rename): a hard crash between the rename and the worker's row
///    insert leaves a row-less dir that the NEXT startup's orphan sweep legitimately
///    reclaims — acceptable degradation (payload dirs are copies; the sources are the
///    user's catalog files; the old row's Resend affordance goes dead and the user
///    re-sends from the library). Documented, not engineered around.
/// 6. **Journal** cross-links on both ids (old: `resend as_new_transfer=<new>`; new:
///    `resend of_declined=<old>`), best-effort.
/// 7. The old declined row is **kept as history**; return the NEW id.
async fn resend_declined_as_new_transfer(
    ctx: &Arc<ServiceContext>,
    sender: &Arc<SyncSenderRuntime>,
    collab_sender: Arc<SyncSenderRuntime>,
    sync: &SyncRuntime,
    row: &OutboundRow,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<i64, ApiError> {
    use crate::sharing::types::AnnounceFileEntry;
    use crate::sync::store::{append_sync_event, outbound_ref_states, rekey_sync_sources};

    let old_id = row.id;
    let old_ref = row.package_ref.clone();
    let old_dir = PathBuf::from(&old_ref);
    let parent = old_dir
        .parent()
        .ok_or_else(|| {
            ApiError::Internal(format!("package {old_id} dir has no parent: {old_ref}"))
        })?
        .to_path_buf();
    let new_dir = parent.join(uuid::Uuid::new_v4().to_string());
    let new_ref = new_dir.to_string_lossy().to_string();

    // Steps 1–4 under ONE pooled connection, dropped before the async enqueue (no DB
    // handle is ever held across an `.await`).
    let (files, display_name, layout) = {
        let database = db(ctx)?;
        let conn = database.conn();

        // (1) Sole-owner guard.
        let shared = outbound_ref_states(&conn)
            .map_err(|e| ApiError::Internal(format!("scan outbound rows: {e:#}")))?
            .into_iter()
            .any(|(other_id, pref, _)| other_id != old_id && pref == old_ref);
        if shared {
            return Err(ApiError::Invalid(format!(
                "package {old_id} shares its payload directory with another transfer; cannot resend as a new transfer"
            )));
        }

        // (4, read before the rename) Clone the manifest for the new row's announce.
        let file_rows = list_outbound_files(&conn, old_id)
            .map_err(|e| ApiError::Internal(format!("list outbound files {old_id}: {e:#}")))?;
        let files: Vec<AnnounceFileEntry> = file_rows
            .iter()
            .map(|r| AnnounceFileEntry {
                rel_path: r.rel_path.clone(),
                byte_size: r.byte_size,
                frame_uuid: r.frame_uuid.clone(),
            })
            .collect();
        let display_name = row.display_name.clone().filter(|s| !s.trim().is_empty());
        // Mirror-hierarchy T2: a declined resend mints a NEW transfer, so it does
        // NOT inherit the stamp by row reuse the way an ordinary resend does —
        // clone it explicitly, alongside the manifest and the name. The user chose
        // this landing shape when the original was queued; a decline is not a
        // re-choice of shape.
        let layout = row.layout;

        (files, display_name, layout)
    };

    // (5a) Resolve row.peer's engine BEFORE any side effect (review fix): engine
    // construction is network/hub-dependent and can fail transiently (relay
    // resolution, node startup, dial address) — if it fails HERE, nothing has been
    // renamed or re-keyed and the old row's Resend stays fully live. Renaming
    // first would leave every engine-resolution failure with no rename-back.
    let engine = match sender.current_for(&row.peer).await {
        Some((engine, _)) => engine,
        None => {
            let (engine, _origin) =
                ensure_sender_engine(ctx, sender, collab_sender, sync, row.peer, None, emitter)
                    .await?;
            engine
        }
    };

    // (2) Rename the payload dir → a fresh batch identity. A NotFound here means
    // the dir is already gone — most likely a concurrent resend of the same row
    // won the race (the rename is the atomic serialization point) — surface the
    // same honest wording as the payload guard instead of a raw OS error.
    if let Err(e) = std::fs::rename(&old_dir, &new_dir) {
        return Err(if e.kind() == std::io::ErrorKind::NotFound {
            ApiError::Invalid(format!(
                "package {old_id} data is missing on disk; cannot resend (already resent as a new transfer?)"
            ))
        } else {
            ApiError::Internal(format!(
                "rename declined package dir for resend ({old_ref} → {new_ref}): {e}"
            ))
        });
    }

    // (3) Re-key retention linkage onto the new dir. Best-effort: a failure only
    // means retention can't reclaim the new transfer's sources later (they stay
    // on disk — the safe direction).
    {
        let database = db(ctx)?;
        let conn = database.conn();
        match rekey_sync_sources(&conn, &old_ref, &new_ref) {
            Ok(moved) => {
                tracing::debug!(
                    old_id,
                    moved,
                    "re-keyed sync_sources onto the resent transfer"
                )
            }
            Err(e) => tracing::warn!(
                old_id,
                error = %format!("{e:#}"),
                "re-key sync_sources for resend failed; retention may not reclaim the new transfer"
            ),
        }
    }
    // The worker inserts the new `sync_outbound` row (+ per-file `pending` rows) in
    // one transaction and replies with its id — do NOT pre-insert here.
    let new_id = match engine
        .enqueue_package(&new_dir, display_name, files, layout)
        .await
    {
        Ok(id) => id,
        Err(e) => {
            // Best-effort undo so the old row's Resend affordance stays live: rename
            // the dir back and re-key the sources back to `old_ref`. (See the crash-
            // window note above — a hard crash inside this window is handled by the
            // startup orphan sweep, so this undo is a courtesy, never a correctness
            // dependency.)
            if std::fs::rename(&new_dir, &old_dir).is_ok() {
                if let Ok(database) = db(ctx) {
                    let conn = database.conn();
                    if let Err(re) = rekey_sync_sources(&conn, &new_ref, &old_ref) {
                        tracing::warn!(old_id, error = %format!("{re:#}"), "re-key sync_sources back after failed resend enqueue");
                    }
                }
            } else {
                tracing::warn!(
                    old_id,
                    "could not rename package dir back after failed resend enqueue"
                );
            }
            return Err(ApiError::Internal(format!(
                "enqueue resent transfer: {e:#}"
            )));
        }
    };

    // (6) Journal cross-links on both ids (best-effort — a log write must never fail
    // the resend). The old row's own terminal `cancelled` is already journaled; the
    // new row's `enqueued`/`announce_sent`/`confirmed` follow from the worker.
    {
        if let Ok(database) = db(ctx) {
            let conn = database.conn();
            if let Err(e) = append_sync_event(
                &conn,
                Direction::Sent,
                &old_id.to_string(),
                "resend",
                Some(&format!("as_new_transfer={new_id}")),
            ) {
                tracing::warn!(package_id = old_id, error = %format!("{e:#}"), "append resend cross-link (old) failed");
            }
            if let Err(e) = append_sync_event(
                &conn,
                Direction::Sent,
                &new_id.to_string(),
                "resend",
                Some(&format!("of_declined={old_id}")),
            ) {
                tracing::warn!(package_id = new_id, error = %format!("{e:#}"), "append resend cross-link (new) failed");
            }
        }
    }

    // (7) Old declined row kept as history; the new transfer is now driving.
    tracing::info!(old_id, new_id, peer = %node_id_hex(&row.peer), "declined transfer resent as new");
    Ok(new_id)
}

/// Backwards-compatible alias for the command/route layer (Transfers Batch Model
/// §D3: `retry_sync_package` keeps its name this wave — B5 owns any surface
/// rename). Delegates to [`resend_transfer`], which resets the SAME row rather than
/// minting a new one.
pub async fn retry_sync_package(
    ctx: &Arc<ServiceContext>,
    sender: &Arc<SyncSenderRuntime>,
    collab_sender: Arc<SyncSenderRuntime>,
    sync: &SyncRuntime,
    id: i64,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<i64, ApiError> {
    resend_transfer(ctx, sender, collab_sender, sync, id, emitter).await
}

/// Send-now a live outbound package: collapse its retry backoff so the owning
/// engine re-announces on the next worker pass (spec §2 wake / send-now, Task 5's
/// `kick`). Only a package currently in flight has a slot to kick —
/// [`active_engine_for_row`] returns `Invalid("package is not active")` for a
/// terminal / unknown id (retry, not send-now, is the terminal-row action).
pub async fn send_now_sync_package(
    sender: &Arc<SyncSenderRuntime>,
    id: i64,
) -> Result<(), ApiError> {
    let engine = active_engine_for_row(sender, id).await?;
    engine
        .kick(id)
        .await
        .map_err(|e| ApiError::Internal(format!("send-now package {id}: {e:#}")))?;
    tracing::info!(package_id = id, "sync package send-now");
    Ok(())
}

/// Cancel a live outbound package: drive it to the terminal `Cancelled` state on
/// its owning engine (Task 3). Only an in-flight package can be cancelled —
/// [`active_engine_for_row`] returns `Invalid("package is not active")` for a
/// terminal / unknown id (an already-terminal package needs no cancel).
pub async fn cancel_sync_package(sender: &Arc<SyncSenderRuntime>, id: i64) -> Result<(), ApiError> {
    let engine = active_engine_for_row(sender, id).await?;
    engine
        .cancel(id)
        .await
        .map_err(|e| ApiError::Internal(format!("cancel package {id}: {e:#}")))?;
    tracing::info!(package_id = id, "sync package cancel requested");
    Ok(())
}

/// Cancel an INBOUND package the receiver is about to fetch or is fetching
/// (Task 12) — the receive-side counterpart of [`cancel_sync_package`]. Desktop
/// only: Perseus never receives, so it has no inbound surface.
///
/// Signals the running receiver's [`InboundControl`](crate::sync::InboundControl)
/// so an in-flight fetch aborts promptly and the receiver runs its cancel epilogue
/// (fetch manifest → write a `Cancelled` receipt per frame → ack → row
/// `Cancelled`); the sender's all-cancelled handler then marks its outbound row
/// `Cancelled`. State-keyed on the persisted `sync_inbound` row:
///
/// - `Ingesting` → refused ([`ApiError::Invalid`] "too late: ingest in progress"):
///   frames are landing in the catalog and there is no clean abort point.
/// - terminal (`Done`/`Failed`/`Cancelled`) → no-op `Ok`.
/// - `Announced` (no in-flight fetch to interrupt) → `request_cancel` + stamp the
///   row `Cancelled` now (restart-proof); the epilogue runs on the sender's next
///   re-announce, which the retry loop guarantees.
/// - `Fetching` → `request_cancel`; the receiver's fetch select-loop notices,
///   aborts the download, and its epilogue owns the terminal `Cancelled` stamp
///   (so this never races the fetch's own state writes).
/// - no row yet (cancel before the announce arrives) → `request_cancel` so the
///   very first announce runs the epilogue. Accepted risk: with no row there is
///   nothing durable to stamp, so this one decline shape does not survive a
///   restart — the window is tiny and the user can decline again.
///
/// **Decline Finality Axis (§D2 primary write)**: every EXISTING row not in
/// `ingesting`/`done` additionally gets `declined_at` stamped HERE, immediately —
/// including a live `Fetching` row (not the `state` column, so it cannot race the
/// fetch loop's state writes) and a raced `cancelled`/`failed` attempt-terminal
/// (the decline of a still-resendable transfer survives losing the race to a
/// sender revoke or fetch failure). The state guard is INSIDE the UPDATE, so a
/// row that advanced to `ingesting`/`done` while the command waited is atomically
/// excluded — delivered data never becomes declined-final. This is what makes
/// the decline crash-durable: even if the epilogue never runs (crash) and a
/// restart reconcile overwrites `state` to `failed`, the transfer stays
/// declined-final for every future re-announce.
///
/// When the transport is not started (`inbound_control` is `None`) there is no
/// live fetch to interrupt; a non-terminal persisted row is still stamped
/// `Cancelled` so a later receiver session self-heals it on re-announce.
pub async fn cancel_incoming_package(
    ctx: &ServiceContext,
    sync: &SyncRuntime,
    package_id: &str,
) -> Result<(), ApiError> {
    use crate::sync::store::{get_inbound, set_inbound_state};
    use crate::sync::InboundState;

    let (_sync_dir, db_path) = sync_paths(ctx)?;
    let store = CatalogSyncStore::open(&db_path)
        .map_err(|e| ApiError::Internal(format!("open catalog sync store: {e:#}")))?;
    let control = sync.inbound_control().await;

    let row = {
        let conn = store.lock_conn();
        get_inbound(&conn, package_id)
            .map_err(|e| ApiError::Internal(format!("read inbound row {package_id}: {e:#}")))?
    };

    // Ingest already underway — refuse (spec §4): no clean abort once frames land.
    if matches!(&row, Some(r) if r.state == InboundState::Ingesting) {
        return Err(ApiError::Invalid(
            "too late: ingest in progress".to_string(),
        ));
    }
    // Decline Finality Axis §D2 (primary write): stamp the transfer-level decline
    // marker NOW, before any flag or state write — safe even during a live fetch
    // (not the `state` column), first-write-wins, survives crash + restart
    // reconcile, and makes every future re-announce of this batch declined-final.
    // The state guard lives INSIDE the UPDATE so decision and write are one
    // atomic statement (no TOCTOU on the earlier row read): a row that raced to
    // `ingesting`/`done` while this command waited must NOT become declined-final
    // — its frames are landing/landed, and stamping it would tell the sender
    // "cancelled by receiver" about a delivered transfer on the next resend.
    // A raced `cancelled` ("by sender") / `failed` attempt-terminal IS stamped:
    // the user's decline of a still-resendable transfer must survive the race
    // with a sender revoke or a fetch failure.
    if let Some(r) = &row {
        let conn = store.lock_conn();
        conn.execute(
            "UPDATE sync_inbound
             SET declined_at = COALESCE(declined_at, ?1)
             WHERE id = ?2 AND state NOT IN ('ingesting', 'done')",
            rusqlite::params![crate::sync::now_iso(), r.id],
        )
        .map_err(|e| {
            ApiError::Internal(format!("stamp declined_at for inbound {}: {e:#}", r.id))
        })?;
    }

    // Already terminal — the decline (if applicable) is recorded above; nothing
    // live remains to signal or stamp.
    if matches!(&row, Some(r) if r.state.is_terminal()) {
        return Ok(());
    }

    // Wake the running receiver (aborts an in-flight fetch; also covers the
    // no-row "cancel before announce" case, where the first announce epilogues).
    if let Some(c) = &control {
        c.request_cancel(package_id);
    }

    // Persist the terminal `Cancelled` row for an `Announced` row now (no live
    // fetch to interrupt), or defensively for any non-terminal row when no live
    // receiver owns it. A live `Fetching` row's stamp is left to its select-loop
    // epilogue so this never races the fetch's own writes.
    let stamp_now = match row.as_ref().map(|r| r.state) {
        Some(InboundState::Announced) => true,
        // D2 §4: a parked row has no live fetch to interrupt and no guaranteed
        // announce coming, so nothing else would ever close it. Stamped here or it
        // keeps `declined_at` and sits `waiting` forever — and a non-terminal row is
        // refused by `delete_transfer_history`, so the user could not get rid of it.
        Some(InboundState::Waiting) => true,
        Some(InboundState::Fetching) => control.is_none(),
        _ => false,
    };
    if stamp_now {
        let conn = store.lock_conn();
        set_inbound_state(&conn, package_id, InboundState::Cancelled, None).map_err(|e| {
            ApiError::Internal(format!("stamp inbound cancelled {package_id}: {e:#}"))
        })?;
    }
    tracing::info!(package_id = %package_id, "sync inbound cancel requested");
    Ok(())
}

/// The count of durable rows removed by [`delete_transfer_history`] — the state
/// rows (`sync_outbound` for sent / `sync_inbound` for received) and the
/// `sync_history` audit rows — for the caller's confirmation toast.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct DeletedTransferRecord {
    /// Number of state rows deleted — `sync_outbound` rows (sent) or `sync_inbound`
    /// rows (received). A batch's attempts share the key, so this is ≥1 for a real
    /// batch and 0 for an already-absent one.
    pub rows: u32,
    /// Number of `sync_history` audit rows deleted for the batch.
    pub history: u32,
}

/// Delete one transfer BATCH's records from the durable store — the Transfers UI's
/// "remove from history" action (Transfers Status Model v2, UX wave 2).
/// Batch-level, not row-level: every attempt of a batch shares `package_key`, so
/// all of that batch's rows go together.
///
/// `package_key` is the batch identity the Transfers UI already holds — the
/// transfer's `batch_uuid` in BOTH directions now (F6 fully unified, B5b): for
/// [`Sent`](Direction::Sent) it is the package-dir BASENAME
/// ([`outbound_package_key`], which B3 aligned to equal the wire `batch_uuid` and
/// which `sync_history.package_id` carries); for [`Received`](Direction::Received)
/// it is the durable `sync_inbound.batch_uuid` (== the received history
/// `package_id` since B5b), matched via [`inbound_id_states_by_batch`] with a
/// NULL-edge fallback to the wire `package_id` so a legacy NULL-batch_uuid row stays
/// deletable by its wire id. (The wire id still rotates per attempt and is used only
/// for blob-tag release / the pre-B5b history sweep, not as the batch key.)
///
/// **Terminal-only, atomically.** If ANY matching row is non-terminal (an active
/// attempt) the whole call refuses with [`ApiError::Invalid`] and deletes NOTHING
/// — cancel the attempt first. The terminal check and every delete run inside ONE
/// `BEGIN IMMEDIATE` transaction on ONE connection, so a concurrently
/// restarted/resurrected attempt cannot flip a row non-terminal between the check
/// and the delete (the IMMEDIATE write lock blocks the engine's writer until we
/// commit or roll back), and a refusal — an early `return` before `commit` — rolls
/// the whole thing back untouched.
///
/// **Delete = reclaim (Transfers Batch Model §D4).** After the records commit, the
/// batch's temp data is reclaimed best-effort — a sent batch's on-disk payload
/// directory (`package_ref`) is removed, and a received batch's `in-flight/` blob
/// tags are released through the SAME `transport.release` the receiver's
/// cancel/revoke paths use. Both run OUTSIDE the transaction, AFTER commit, and a
/// failure only `warn!`s: reclaim never turns a successful record delete into an
/// error. The sent-dir removal is unconditional-safe because every row sharing the
/// dir basename (== `package_key`) was just deleted, so no remaining row references
/// it. It still does NOT touch the catalog (`files`/`frames`) or the received files
/// on disk.
pub async fn delete_transfer_history(
    ctx: &ServiceContext,
    sync: &SyncRuntime,
    direction: Direction,
    package_key: String,
) -> Result<DeletedTransferRecord, ApiError> {
    use crate::sharing::types::PackageId;
    use crate::sync::store::{
        cancel_outbound_row, delete_history_for_package, delete_inbound_files, delete_inbound_row,
        delete_outbound_files, delete_outbound_row, delete_sync_events, inbound_id_states_by_batch,
        outbound_peer_hex, outbound_ref_states, settle_unsettled_outbound_files,
    };
    use crate::sync::InboundState;

    // Temp-data reclaim targets captured during the guarded DB pass and executed
    // once, AFTER commit (never inside the tx — fs / blob-store ops can't roll
    // back, so we reclaim only when the records are durably gone).
    enum Reclaim {
        /// Sent: on-disk payload directories (`package_ref`) to remove.
        Payloads(Vec<String>),
        /// Received: wire ids whose permanent + `in-flight/` blob tags to release.
        Tags(Vec<String>),
    }

    // The whole guarded DB pass runs in an inner block so the pooled connection and
    // the (`!Send`) transaction are dropped at its closing brace — BEFORE the async
    // reclaim below — keeping the command's future `Send` (no DB handle is ever held
    // across an await).
    let (result, reclaim) = {
        let db = db(ctx)?;
        let mut conn = db.conn();
        // BEGIN IMMEDIATE: take the reserved write lock up front so the terminal
        // guard and the deletes are atomic against a concurrently resurrected
        // attempt (the engine's writer blocks until we commit/rollback). An early
        // `return Err` (the non-terminal refusal) or any `?` failure drops the tx →
        // rollback → no partial delete.
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| ApiError::Internal(format!("begin delete_transfer_history tx: {e}")))?;

        let out = match direction {
            Direction::Sent => {
                // Group this batch's attempts: every outbound row whose package_ref
                // basename equals the key (Rust-side compare — no LIKE). Post-wipe this
                // is EXACTLY the transfer's own rows (basename == batch_uuid, unique).
                let matching_full: Vec<(i64, String, OutboundState)> = outbound_ref_states(&tx)?
                    .into_iter()
                    .filter(|(_, package_ref, _)| outbound_package_key(package_ref) == package_key)
                    .map(|(id, package_ref, state)| {
                        Ok::<_, ApiError>((id, package_ref, OutboundState::from_db(&state)?))
                    })
                    .collect::<Result<_, _>>()?;
                let matching: Vec<(i64, OutboundState)> = matching_full
                    .iter()
                    .map(|(id, _, state)| (*id, *state))
                    .collect();
                // The distinct payload dirs to reclaim after commit (deduped — all
                // attempts of one transfer share one dir post batch-model).
                let mut payload_dirs: Vec<String> = Vec::new();
                for (_, package_ref, _) in &matching_full {
                    if !payload_dirs.contains(package_ref) {
                        payload_dirs.push(package_ref.clone());
                    }
                }

                // Split the non-terminal (active-attempt) rows by peer liveness. A row
                // whose peer has LEFT the account can never complete: the resurrection
                // account-gate (1a645e17) skips it, so it has no engine, is invisible in
                // the active list, and cannot be cancelled — the old terminal-only guard
                // then blocked this batch's record deletion forever. Read each
                // non-terminal row's peer as the RAW stored hex (no `NodeId` parse) so a
                // malformed legacy row can't abort the delete.
                let mut non_terminal: Vec<(i64, OutboundState, String)> = Vec::new();
                for (id, state) in &matching {
                    if !state.is_terminal() {
                        let peer_hex = outbound_peer_hex(&tx, *id)?.unwrap_or_default();
                        non_terminal.push((*id, *state, peer_hex));
                    }
                }
                if !non_terminal.is_empty() {
                    // Reuse the EXACT cached hex allow-list the resurrection gate reads
                    // (a local-DB peek — NO hub call). This grabs a second pooled
                    // connection; in WAL mode its read never blocks on our `IMMEDIATE`
                    // write lock, and `settings` is untouched by this tx.
                    let authorized = cached_authorized_peer_hexes(ctx);
                    if authorized.is_empty() {
                        // No device data at all — a cold/never-populated cache. Never
                        // guess about liveness with no data: refuse and retry later.
                        return Err(ApiError::Invalid(
                            "device list unavailable — retry after the account refreshes".into(),
                        ));
                    }
                    // Any non-terminal row whose peer is STILL a live account device is a
                    // genuine in-flight attempt — refuse and name the state so the UI can
                    // point the user at the live row. Check ALL before cancelling ANY
                    // (all-or-refuse, atomic inside this tx).
                    if let Some((_, state, _)) = non_terminal
                        .iter()
                        .find(|(_, _, peer)| authorized.contains(peer))
                    {
                        return Err(ApiError::Invalid(format!(
                            "batch has an active attempt ({}) — cancel it in Sending/Waiting first",
                            state.as_str()
                        )));
                    }
                    // Every remaining non-terminal row is a dead-peer orphan (peer absent
                    // from a NON-EMPTY account set) → transition each to `cancelled`
                    // in-place before the batch delete below. The state write + file
                    // settle are moot on their own (both rows are deleted a few lines down
                    // in THIS tx) but keep the honest cancel-then-delete semantic; the
                    // `sync_events` journal write is deliberately SKIPPED — the immediate
                    // `delete_sync_events` below would erase it in the same tx — while the
                    // durable audit trail is the `info!` here.
                    for (id, _, peer_hex) in &non_terminal {
                        cancel_outbound_row(&tx, *id)?;
                        settle_unsettled_outbound_files(&tx, *id, Some("cancelled"))?;
                        tracing::info!(
                            package_key = %package_key,
                            peer = %peer_hex,
                            "orphaned attempt cancelled during delete (peer left account)"
                        );
                    }
                }
                let mut rows = 0u32;
                for (id, _) in &matching {
                    delete_outbound_files(&tx, *id)?;
                    delete_sync_events(&tx, Direction::Sent, &id.to_string())?;
                    delete_outbound_row(&tx, *id)?;
                    rows += 1;
                }
                let history = delete_history_for_package(&tx, Direction::Sent, &package_key)?;
                (
                    DeletedTransferRecord { rows, history },
                    Reclaim::Payloads(payload_dirs),
                )
            }
            Direction::Received => {
                // Batch-keyed on the durable `batch_uuid` (B5b, symmetric with the sent
                // side keying on the package-dir basename), with a NULL-edge fallback to
                // the wire `package_id` so a legacy NULL-batch_uuid row stays deletable by
                // its wire id. Each matched row also yields its CURRENT wire id (for tag
                // release + the wire-id history sweep below).
                let matched = inbound_id_states_by_batch(&tx, &package_key)?;
                let matching: Vec<(i64, InboundState)> = matched
                    .iter()
                    .map(|(id, state, _)| Ok::<_, ApiError>((*id, InboundState::from_db(state)?)))
                    .collect::<Result<_, _>>()?;
                if matching.iter().any(|(_, s)| !s.is_terminal()) {
                    return Err(ApiError::Invalid(
                        "batch has an active attempt — cancel it first".into(),
                    ));
                }
                let mut rows = 0u32;
                for (id, _) in &matching {
                    delete_inbound_files(&tx, *id)?;
                    delete_sync_events(&tx, Direction::Received, &id.to_string())?;
                    delete_inbound_row(&tx, *id)?;
                    rows += 1;
                }
                // Received history rows now key on the `batch_uuid` (== `package_key`,
                // B5b). Also sweep by each matched row's CURRENT wire id to catch any
                // rows written wire-id-keyed before B5b landed in the same dev cycle —
                // post-wipe there is no data older than this wave, so this is dev-cycle
                // hygiene only, not a migration.
                let mut history =
                    delete_history_for_package(&tx, Direction::Received, &package_key)?;
                let wire_ids: Vec<String> =
                    matched.iter().map(|(_, _, wire)| wire.clone()).collect();
                for wire in &wire_ids {
                    if *wire != package_key {
                        history += delete_history_for_package(&tx, Direction::Received, wire)?;
                    }
                }
                // Release the matched rows' CURRENT wire ids' blob tags (the batch key is
                // the stable `batch_uuid` now, which is NOT a tag — the wire id is).
                (
                    DeletedTransferRecord { rows, history },
                    Reclaim::Tags(wire_ids),
                )
            }
        };

        tx.commit()
            .map_err(|e| ApiError::Internal(format!("commit delete_transfer_history: {e}")))?;
        out
    };

    // Best-effort temp-data reclaim (§D4). NEVER fails the delete: the records are
    // already durably gone; a reclaim error is logged and swallowed.
    match reclaim {
        Reclaim::Payloads(dirs) => {
            for dir in &dirs {
                // A confirmed/old batch's payload may already be gone (retention swept
                // it) — that is the common case and not worth a warning; only a real
                // removal failure of a still-present dir warns.
                if std::path::Path::new(dir).exists() {
                    if let Err(e) = std::fs::remove_dir_all(dir) {
                        tracing::warn!(package_key = %package_key, path = %dir, error = %e, "delete reclaim: payload dir removal failed");
                    } else {
                        tracing::info!(package_key = %package_key, path = %dir, "delete reclaim: payload dir removed");
                    }
                }
            }
        }
        Reclaim::Tags(wire_ids) => {
            // Release the batch's `in-flight/` + permanent blob tags through the live
            // receive-side transport (the same mechanism the receiver's cancel/revoke
            // paths use). No live transport (not started) → nothing to release. A
            // missing/already-released tag is a silent no-op inside `release`.
            if !wire_ids.is_empty() {
                if let Some(transport) = sync.transport().await {
                    for wire in &wire_ids {
                        if let Err(e) = transport.release(&PackageId(wire.clone())).await {
                            tracing::warn!(package_key = %package_key, wire_id = %wire, error = %format!("{e:#}"), "delete reclaim: blob-tag release failed");
                        }
                    }
                }
            }
        }
    }

    tracing::info!(
        direction = direction.as_str(),
        package_key = %package_key,
        rows = result.rows,
        history = result.history,
        "transfer history deleted"
    );
    Ok(result)
}

// ── Transfer storage: startup orphan sweep + stats + cleanup (Batch Model §D4, B7) ──

/// A directory's total on-disk size — the recursive sum of regular-file lengths.
/// Best-effort: an unreadable entry (permission / vanished mid-walk) contributes
/// 0 and never fails, so a stats/cleanup pass never aborts on one bad file.
fn dir_size_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(dir).into_iter().flatten() {
        if entry.file_type().is_file() {
            if let Ok(md) = entry.metadata() {
                total += md.len();
            }
        }
    }
    total
}

/// The set of package-dir BASENAMES ([`outbound_package_key`]) referenced by ANY
/// `sync_outbound` row — terminal or not. A `<sync>/packages/<name>` dir whose
/// basename is in this set is referenced by some row (a terminal row keeps its
/// payload for Resend; a non-terminal row for its live attempt) and is therefore
/// NOT a row-less orphan. Basename-keyed, not full-path: every package dir is
/// minted `<sync>/packages/<uuid>` so the basename is the unique batch identity,
/// robust against `/Volumes` vs `/private/Volumes` path normalization. Read fresh
/// at sweep time (not cached at boot) so a package a concurrent RESURRECTION
/// needs (it only re-drives EXISTING non-terminal rows, never creates a new dir)
/// is already protected.
///
/// **This does NOT by itself protect a concurrent user ENQUEUE** (review fix,
/// Critical): `write_package` creates and populates `packages/<uuid>`
/// incrementally — manifest written LAST, seconds-to-minutes for a multi-GB
/// batch — and the engine worker inserts the `sync_outbound` row only once that
/// build is done. A dir can therefore be genuinely row-less (absent from THIS
/// set, however freshly read) while mid-build, or for the instant between the
/// row's commit and this read. The age-gate in
/// [`sweep_orphan_payload_dirs`] is what protects that case — see its doc.
fn referenced_package_keys(ctx: &ServiceContext) -> Result<HashSet<String>, ApiError> {
    use crate::sync::store::outbound_ref_states;
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(outbound_ref_states(&conn)?
        .into_iter()
        .map(|(_, package_ref, _)| outbound_package_key(&package_ref))
        .collect())
}

/// Orphan payload-dir sweep age gate (B7, review fix — Critical). A row-less dir
/// younger than this is presumed either an in-progress enqueue (dir-before-row:
/// `write_package` builds `packages/<uuid>` incrementally, manifest written LAST,
/// seconds-to-minutes for a multi-GB batch, before the engine worker inserts the
/// `sync_outbound` row) or one whose row just landed after our DB snapshot — never
/// a genuine orphan. A genuine orphan is leftover from a PRIOR session, so 5
/// minutes is conservative in both directions: comfortably longer than any
/// realistic per-file write gap within one package build, and far shorter than
/// "since last restart". See [`sweep_orphan_payload_dirs`].
const ORPHAN_SWEEP_MIN_AGE: Duration = Duration::from_secs(5 * 60);

/// A directory's on-disk modification age (`now - mtime`), taken from the MOST
/// RECENTLY modified entry ANYWHERE in its subtree (the dir itself and every file
/// under it, recursively via `walkdir` — the same walk [`dir_size_bytes`] uses),
/// not just the top-level dir's own mtime.
///
/// The top-level dir's own mtime only advances on ENTRY creation/removal within
/// it, not on writes to an existing file's content — so a single large in-flight
/// file (e.g. a multi-GB master FITS copied in over NAS-backed storage) that takes
/// longer than [`ORPHAN_SWEEP_MIN_AGE`] to finish would freeze the dir's own mtime
/// while genuinely live write activity continues underneath it, and the sweep
/// could bite mid-copy. Reading the recursive MAX mtime instead tracks that file's
/// own mtime directly, which a live write keeps advancing — closing the window
/// outright rather than merely narrowing it.
///
/// `None` only if EVERY entry's mtime is unreadable (vanished mid-walk / a
/// filesystem without mtime support), in which case the caller skips rather than
/// guesses.
fn dir_age(path: &Path) -> Option<Duration> {
    let now = SystemTime::now();
    let mut newest: Option<SystemTime> = None;
    for entry in walkdir::WalkDir::new(path).into_iter().flatten() {
        if let Ok(md) = entry.metadata() {
            if let Ok(modified) = md.modified() {
                newest = Some(newest.map_or(modified, |cur| cur.max(modified)));
            }
        }
    }
    newest.and_then(|m| now.duration_since(m).ok())
}

/// Startup pass 1 — remove ROW-LESS orphan payload dirs under `<sync>/packages/`:
/// a dir whose basename matches NO `sync_outbound` row (an interrupted build, or a
/// dir whose rows were deleted without their reclaim landing). Never touches a dir
/// referenced by any row (terminal rows keep payload for Resend — only truly
/// row-less orphans go).
///
/// **Age-gated (review fix, Critical).** Row-presence alone is NOT sufficient to
/// call a dir orphaned: the enqueue path is dir-before-row (see
/// [`ORPHAN_SWEEP_MIN_AGE`]), so a fresh-at-removal-time DB read
/// ([`referenced_package_keys`]) can still observe a populated, in-progress send's
/// dir with no row yet — deleting it would destroy a live send. Every row-less dir
/// is therefore additionally checked against [`ORPHAN_SWEEP_MIN_AGE`] via
/// [`dir_age`] and skipped (at `debug!`) if younger; an unreadable mtime is also
/// skipped (at `warn!`), never guessed at. A genuine orphan simply survives to the
/// next sweep once it ages past the gate. Returns `(dirs_removed,
/// bytes_reclaimed)`. Best-effort: one un-removable dir `warn!`s and the pass
/// continues.
fn sweep_orphan_payload_dirs(ctx: &ServiceContext) -> Result<(u32, u64), ApiError> {
    let packages_dir = sender_packages_dir(ctx)?;
    // Fresh DB read AT removal time: resurrection never deletes rows, so any dir a
    // resurrected row needs is in this set → never removed. (Does NOT by itself
    // cover a concurrent enqueue — see the age-gate below and this fn's doc.)
    let referenced = referenced_package_keys(ctx)?;
    let entries = match std::fs::read_dir(&packages_dir) {
        Ok(e) => e,
        // No packages dir yet (never sent) → nothing to sweep.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(e) => return Err(ApiError::Internal(format!("read packages dir: {e}"))),
    };
    let mut dirs = 0u32;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if referenced.contains(name) {
            continue; // referenced by some row → keep
        }
        // Age-gate (review fix, Critical): a row-less dir this young is presumed
        // in-progress (dir-before-row enqueue window) or just-committed (its row
        // landed after our `referenced` snapshot) — never treated as an orphan.
        match dir_age(&path) {
            Some(age) if age < ORPHAN_SWEEP_MIN_AGE => {
                tracing::debug!(
                    path = %path.display(),
                    age_secs = age.as_secs(),
                    "orphan sweep: dir too young, skipped (in-progress enqueue guard)"
                );
                continue;
            }
            Some(_) => {}
            None => {
                tracing::warn!(path = %path.display(), "orphan sweep: mtime unreadable, skipped");
                continue;
            }
        }
        let sz = dir_size_bytes(&path);
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                dirs += 1;
                bytes += sz;
                tracing::info!(path = %path.display(), "orphan payload dir removed");
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "orphan payload dir removal failed")
            }
        }
    }
    Ok((dirs, bytes))
}

/// Cleanup pass 1 — remove the payload dirs of TERMINAL outbound rows
/// (confirmed / failed / cancelled), the explicit "reclaim finished transfers"
/// action. A dir also referenced by a NON-terminal row of the same batch (attempts
/// share one dir post batch-model) is KEPT — a live attempt's payload is never
/// touched. Removing a terminal row's payload honestly flips its summary
/// `resendable` to false (that flag is derived from [`package_has_payload`], not
/// stored). Returns `(dirs_removed, bytes_reclaimed)`.
fn remove_terminal_payload_dirs(ctx: &ServiceContext) -> Result<(u32, u64), ApiError> {
    use crate::sync::store::outbound_ref_states;
    let rows = {
        let db = db(ctx)?;
        let conn = db.conn();
        outbound_ref_states(&conn)?
    };
    let mut non_terminal_dirs: HashSet<String> = HashSet::new();
    let mut terminal_dirs: HashSet<String> = HashSet::new();
    for (_, package_ref, state) in &rows {
        let st = OutboundState::from_db(state).map_err(|e| ApiError::Internal(e.to_string()))?;
        if st.is_terminal() {
            terminal_dirs.insert(package_ref.clone());
        } else {
            non_terminal_dirs.insert(package_ref.clone());
        }
    }
    let mut dirs = 0u32;
    let mut bytes = 0u64;
    for package_ref in terminal_dirs {
        if non_terminal_dirs.contains(&package_ref) {
            continue; // shared with a live attempt → never touch a non-terminal payload
        }
        let path = Path::new(&package_ref);
        if !path.exists() {
            continue; // already reclaimed (delete) / retention-swept / never materialized
        }
        let sz = dir_size_bytes(path);
        match std::fs::remove_dir_all(path) {
            Ok(()) => {
                dirs += 1;
                bytes += sz;
                tracing::info!(path = %package_ref, "finished-transfer payload dir removed");
            }
            Err(e) => {
                tracing::warn!(path = %package_ref, error = %e, "finished-transfer payload dir removal failed")
            }
        }
    }
    Ok((dirs, bytes))
}

/// Cleanup pass 2 — remove the RECEIVE-side staging trees of finished batches
/// (D2 §3.5).
///
/// [`remove_terminal_payload_dirs`] walks `sync_outbound`, so on a receive-only
/// device the Clean-up button had nothing in scope BY CONSTRUCTION — while three
/// receiver failure paths (fetch, ingest, ack) return without removing
/// `<sync>/staging/<wire_id>`, leaving a full second copy of the batch on disk with
/// nothing to reclaim it. This is that reclaimer.
///
/// A directory is removed when its wire id belongs to a TERMINAL `sync_inbound`
/// row or to no row at all. A non-terminal row's staging is kept — INCLUDING a
/// [`Waiting`](crate::sync::InboundState::Waiting) row's, which is precisely its
/// resume target. (That is the opposite of the in-flight TAG rule in
/// [`release_orphan_in_flight_tags`], and deliberately so: the tag is a GC root
/// pinning blob bytes we can re-fetch, the staging tree is the landed data itself.)
///
/// Returns `(dirs_removed, bytes_reclaimed)` — freed-now bytes, unlike released
/// tags. Best-effort: one un-removable dir `warn!`s and the pass continues.
fn remove_terminal_staging_dirs(ctx: &ServiceContext) -> Result<(u32, u64), ApiError> {
    let (sync_dir, _db_path) = sync_paths(ctx)?;
    let staging_root = sync_dir.join("staging");
    let entries = match std::fs::read_dir(&staging_root) {
        Ok(e) => e,
        // Never materialized (nothing ever received) → nothing to reclaim.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(e) => return Err(ApiError::Internal(format!("read staging dir: {e}"))),
    };
    let live: HashSet<String> = {
        let db = db(ctx)?;
        let conn = db.conn();
        inbound_active(&conn)?
            .into_iter()
            .map(|r| r.package_id)
            .collect()
    };
    let mut dirs = 0u32;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(wire) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if live.contains(wire) {
            continue; // a live or parked attempt owns this tree
        }
        let sz = dir_size_bytes(&path);
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                dirs += 1;
                bytes += sz;
                tracing::info!(path = %path.display(), "terminal staging tree removed");
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "staging tree removal failed")
            }
        }
    }
    Ok((dirs, bytes))
}

/// Release receiver-side `in-flight/recv/pkg/<id>` download tags whose transfer is
/// no longer non-terminal, so GC can reclaim the verified partial blobs (Batch
/// Model §D4; B0 findings Q4/R2). Shared by the startup sweep and the cleanup
/// command. For every in-flight tag the live receive-side transport holds
/// ([`SharingTransport::list_in_flight_tags`], already namespace-filtered to the
/// transfer machinery's own tags), release it UNLESS a LIVE `sync_inbound` row
/// still carries that exact wire id — the tag then belongs to a live/resumable
/// fetch and is kept. Returns the number released. No live transport (receiver not
/// started) → 0 (nothing to enumerate or release). Best-effort per tag: a release
/// failure `warn!`s and the pass continues.
///
/// "Live" is the non-terminal rows MINUS [`InboundState::Waiting`] (D2 §3.5): a
/// parked row is non-terminal but has no fetch in flight, and letting it hold its
/// tag would pin a partial download indefinitely — Clean-up would free strictly
/// less than it did before D2.
///
/// **GC note:** iroh-blobs 0.103 exposes no on-demand GC trigger reachable from our
/// layer (`gc_run_once` is `pub` but lives in the private `store::gc` module; only
/// `GcConfig`/`ProtectCb`/`ProtectOutcome` are re-exported). Releasing the tag
/// removes the GC root immediately; the blob bytes are physically reclaimed on the
/// store's next periodic sweep (`GC_INTERVAL`, ~15 min).
async fn release_orphan_in_flight_tags(
    ctx: &ServiceContext,
    sync: &SyncRuntime,
) -> Result<u32, ApiError> {
    let Some(transport) = sync.transport().await else {
        return Ok(0);
    };
    let in_flight = transport
        .list_in_flight_tags()
        .await
        .map_err(|e| ApiError::Internal(format!("list in-flight tags: {e:#}")))?;
    if in_flight.is_empty() {
        return Ok(0);
    }
    // Fresh read of the non-terminal inbound wire ids, in an inner block so the
    // pooled connection drops before the release awaits (keeps the future `Send`).
    // Re-checked here (not snapshotted at boot) so a fetch that just created its
    // inbound row + in-flight tag is never released out from under itself: the
    // receiver writes the non-terminal row BEFORE it sets the in-flight tag.
    let active: HashSet<String> = {
        let db = db(ctx)?;
        let conn = db.conn();
        inbound_active(&conn)?
            .into_iter()
            // D2 §3.5: `Waiting` is non-terminal but NOT live. Keeping its tag would
            // pin a partial download indefinitely — Clean-up would free strictly
            // less than it did before D2, the opposite of what D2 is for. The dedup
            // handshake renegotiates the file list on the next attempt anyway, so at
            // worst the peer re-sends what the GC reclaimed; an unbounded pin the
            // user cannot clear is the worse trade.
            .filter(|r| r.state != crate::sync::InboundState::Waiting)
            .map(|r| r.package_id)
            .collect()
    };
    let mut released = 0u32;
    for pid in in_flight {
        if active.contains(&pid.0) {
            continue; // a live/resumable fetch owns this tag → keep it
        }
        match transport.release(&pid).await {
            Ok(()) => released += 1,
            Err(e) => tracing::warn!(
                wire_id = %pid.0,
                error = %format!("{e:#}"),
                "orphan in-flight tag release failed"
            ),
        }
    }
    Ok(released)
}

/// Fire-and-forget startup orphan sweep (Batch Model §D4, B7) — spawned AFTER
/// [`SyncRuntime::ensure_started`] at every autostart site, same
/// pattern/placement as [`resurrect_pending_senders`]. Two reclaim passes:
/// row-less orphan payload dirs under `<sync>/packages/`, and orphaned receiver
/// in-flight blob tags. Silent when nothing is found; one `info!` with the totals
/// otherwise. Never fails: each pass is best-effort and a pass error only `warn!`s.
///
/// **Concurrent-writer safety (the constraint's race choices):** the sweep runs
/// concurrently with TWO other writers under `<sync>/packages/` — the resurrection
/// spawn (also fired post-`ensure_started`) and a live user enqueue
/// (`write_package` can run at any time, including seconds after boot on the
/// `get_pairing_ticket` re-run path) — and no lock/ordering is used against
/// either. They need different protection because they touch the DB/dir
/// differently:
/// - **Resurrection** never creates a new dir or deletes a row — it only
///   re-drives EXISTING non-terminal rows — so the payload-dir pass's
///   fresh-at-removal-time read ([`referenced_package_keys`]) is sufficient on its
///   own: any dir a resurrected row needs is already in that set.
/// - **A concurrent enqueue** DOES create a new row-less dir first
///   (dir-before-row: `write_package` populates `packages/<uuid>` incrementally,
///   and the `sync_outbound` row lands only once that build is done), so a fresh
///   DB read cannot see a row that does not exist yet. The payload-dir pass
///   additionally age-gates every row-less dir
///   ([`ORPHAN_SWEEP_MIN_AGE`]/[`dir_age`], see [`sweep_orphan_payload_dirs`])
///   rather than relying on row-presence alone.
/// The in-flight-tag pass keys on non-terminal `sync_inbound` rows, which neither
/// writer touches, so it needs no age gate.
pub async fn sweep_transfer_orphans(ctx: Arc<ServiceContext>, sync: Arc<SyncRuntime>) {
    let (payload_dirs, bytes_reclaimed) = match sweep_orphan_payload_dirs(&ctx) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "transfer orphan sweep: payload-dir pass failed");
            (0, 0)
        }
    };
    let tags_released = match release_orphan_in_flight_tags(&ctx, &sync).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "transfer orphan sweep: in-flight tag pass failed");
            0
        }
    };
    if payload_dirs > 0 || bytes_reclaimed > 0 || tags_released > 0 {
        tracing::info!(
            payload_dirs,
            bytes_reclaimed,
            tags_released,
            "transfer orphan sweep"
        );
    }
}

/// On-disk footprint of the transfer machinery's temp data (Batch Model §D4, B7),
/// for the Settings "Transfer storage" line. Walks `<sync>/packages/` (one entry
/// per outbound package dir) and sums the blob store dir `<sync>/blobs/`.
///
/// `blobs_bytes` is a **directory walk of `<sync>/blobs`** — iroh-blobs 0.103
/// exposes no store-size accessor on the public `Store` API, and the walk is the
/// honest on-disk figure (partial + complete blobs alike). It includes bytes still
/// pinned by not-yet-swept GC roots, so right after a cleanup it can lag the
/// eventual reclaimed size by up to one GC window (~15 min).
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct TransferStorage {
    /// Total bytes across every `<sync>/packages/<uuid>` payload dir.
    pub packages_bytes: u64,
    /// Number of top-level package dirs under `<sync>/packages/`.
    pub packages_count: u32,
    /// Total bytes under the blob store dir `<sync>/blobs/` (directory walk).
    pub blobs_bytes: u64,
    /// Total bytes under `<sync>/staging/` — the RECEIVE side's in-progress and
    /// abandoned batch trees (D2 §3.5). Counted separately from `packages_bytes`,
    /// which is the SEND side's payloads, so the figure the Clean-up button sits
    /// next to includes what that button can actually move: on a receive-only
    /// device the payload pass has nothing in scope by construction.
    pub staging_bytes: u64,
}

/// Compute the [`TransferStorage`] footprint (see its doc). Pure fs walk — never
/// touches the DB or the transport; a missing dir reads as empty (0).
pub fn get_transfer_storage(ctx: &ServiceContext) -> Result<TransferStorage, ApiError> {
    let (sync_dir, _db_path) = sync_paths(ctx)?;
    let packages_dir = sync_dir.join("packages");
    let blobs_dir = sync_dir.join("blobs");

    let mut packages_count = 0u32;
    let mut packages_bytes = 0u64;
    if let Ok(entries) = std::fs::read_dir(&packages_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                packages_count += 1;
                packages_bytes += dir_size_bytes(&entry.path());
            }
        }
    }
    let blobs_bytes = dir_size_bytes(&blobs_dir);
    let staging_bytes = dir_size_bytes(&sync_dir.join("staging"));
    Ok(TransferStorage {
        packages_bytes,
        packages_count,
        blobs_bytes,
        staging_bytes,
    })
}

/// The reclaim totals returned by [`cleanup_finished_transfers`] — for the
/// Settings result toast.
///
/// **GC note (mirrors the fn doc):** `payload_bytes` is reclaimed immediately;
/// `tags_released` counts the receiver in-flight blob tags whose GC root was
/// dropped, but iroh-blobs 0.103 has no on-demand GC trigger reachable from our
/// layer, so the blob bytes those tags pinned come back within the store's
/// periodic GC window (~15 min), NOT in this result. Only the payload bytes are
/// reflected here as freed-now.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct TransferCleanup {
    /// Terminal-transfer payload dirs removed from `<sync>/packages/`.
    pub payload_dirs: u32,
    /// Bytes reclaimed by removing those payload dirs (freed immediately).
    pub payload_bytes: u64,
    /// Orphan receiver in-flight blob tags released (blob bytes reclaimed by the
    /// next periodic GC sweep, ~15 min — see the type doc).
    pub tags_released: u32,
    /// Receive-side staging trees of finished batches removed from
    /// `<sync>/staging/` (D2 §3.5). Freed immediately, like `payload_dirs`.
    pub staging_dirs: u32,
    /// Bytes reclaimed by removing those staging trees (freed immediately).
    pub staging_bytes: u64,
}

/// Reclaim finished transfers' temp data on demand (Batch Model §D4, B7; receive
/// side added by D2 §3.5) — the Settings "Clean up finished transfers" action.
/// Three passes: TERMINAL outbound rows' payload dirs (never a non-terminal row's
/// — a live attempt's payload is kept), terminal/row-less receive-side staging
/// trees, and orphan receiver in-flight blob tags. Records are untouched — this
/// reclaims disk only.
///
/// Payload and staging bytes are freed immediately; only `tags_released` is
/// delayed by the store's GC window — see [`TransferCleanup`]. The staging pass is
/// what makes this button do anything at all on a RECEIVE-ONLY device, where the
/// payload pass has nothing in scope by construction (it reads `sync_outbound`).
pub async fn cleanup_finished_transfers(
    ctx: &ServiceContext,
    sync: &SyncRuntime,
) -> Result<TransferCleanup, ApiError> {
    let (payload_dirs, payload_bytes) = remove_terminal_payload_dirs(ctx)?;
    let (staging_dirs, staging_bytes) = remove_terminal_staging_dirs(ctx)?;
    let tags_released = release_orphan_in_flight_tags(ctx, sync).await?;
    tracing::info!(
        payload_dirs,
        payload_bytes,
        staging_dirs,
        staging_bytes,
        tags_released,
        "finished-transfer cleanup"
    );
    Ok(TransferCleanup {
        payload_dirs,
        payload_bytes,
        tags_released,
        staging_dirs,
        staging_bytes,
    })
}

/// Whether full-app capture-node auto mode is enabled (`sync.auto_mode`).
pub fn get_sync_auto_mode(ctx: &ServiceContext) -> Result<bool, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let v =
        ctx.settings
            .get_with_precedence(&conn, keys::SYNC_AUTO_MODE, defaults::SYNC_AUTO_MODE)?;
    Ok(v.eq_ignore_ascii_case("true"))
}

/// Toggle full-app capture-node auto mode.
pub fn set_sync_auto_mode(ctx: &ServiceContext, enabled: bool) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    crate::db::set_setting(
        &conn,
        keys::SYNC_AUTO_MODE,
        if enabled { "true" } else { "false" },
    )?;
    tracing::info!(enabled, "sync auto mode set");
    Ok(())
}

/// Bounds check for [`set_sync_upload_limit`], split out so the real guard
/// (not a test-local copy) is what tests pin — the same shape
/// `api::compute::validate_max_concurrent` uses.
///
/// `0` is the unlimited sentinel and always passes. Any real cap must clear a
/// 100 KB/s floor: below it a transfer stops looking slow and starts looking
/// dead — a single frame would take hours and every progress bar would read as
/// stalled — so the setting could strand the device's sync rather than tame it.
pub(crate) fn validate_upload_limit(bytes_per_sec: u64) -> Result<(), ApiError> {
    if bytes_per_sec != 0 && bytes_per_sec < 100_000 {
        return Err(ApiError::Invalid(
            "sync.max_upload_bytes_per_sec must be 0 (unlimited) or >= 100000".into(),
        ));
    }
    Ok(())
}

/// Persist and live-apply the device-wide sync UPLOAD limit in bytes/sec
/// (`0` = unlimited; see [`validate_upload_limit`] for the floor).
///
/// One budget for the whole device: every role handle, every peer and every
/// concurrent GET draw on the shared node's pacer, so a change lands
/// mid-transfer on the next ~16 KiB chunk the provider offers. When no node is
/// bound yet there is nothing to re-rate and nothing to do — [`ensure_iroh_node`]
/// applies the persisted value at bind, so this deliberately does NOT force a
/// bind (and a whole endpoint + relay resolution) just to record a number.
pub async fn set_sync_upload_limit(
    ctx: &ServiceContext,
    bytes_per_sec: u64,
) -> Result<(), ApiError> {
    validate_upload_limit(bytes_per_sec)?;
    // Scoped so the connection guard is dropped before the `iroh_node` await.
    {
        let db = db(ctx)?;
        let conn = db.conn();
        ctx.settings
            .persist_setting(
                &conn,
                keys::SYNC_MAX_UPLOAD_BYTES_PER_SEC,
                &bytes_per_sec.to_string(),
            )
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    if let Some(node) = ctx.iroh_node.lock().await.as_ref() {
        node.set_upload_limit(bytes_per_sec);
    }
    tracing::info!(bytes_per_sec, "sync upload limit set");
    Ok(())
}

/// Bounds check for [`set_sync_max_concurrent_receives`], split out so the real
/// guard (not a test-local copy) is what tests pin — the same shape
/// [`validate_upload_limit`] and `api::compute::validate_max_concurrent` use.
///
/// `0` would admit nobody: every inbound transfer would park at the gate
/// forever, which reads to the operator as a dead receiver rather than a
/// throttled one. Above 8 the lanes only contend for the same disk — the
/// fetch+ingest phase is disk-bound, so more lanes buy seek thrash, not
/// throughput.
pub(crate) fn validate_max_concurrent_receives(n: usize) -> Result<(), ApiError> {
    if n == 0 || n > 8 {
        return Err(ApiError::Invalid(
            "sync.max_concurrent_receives must be 1..=8".into(),
        ));
    }
    Ok(())
}

/// Persist and live-apply the cap on simultaneous INCOMING transfers (W2 T2.7;
/// see [`validate_max_concurrent_receives`] for the range).
///
/// Both halves matter and neither substitutes for the other: the persisted row
/// is what the NEXT receiver start reads (`receiver_hooks` →
/// [`SyncRuntime::ensure_started`](crate::sync::SyncRuntime::ensure_started)),
/// while the live [`ReceiveGate`](crate::sync::ReceiveGate) resize is what makes
/// the change land on the receiver running right now — without restarting it.
/// A shrink never interrupts a transfer already holding a lane; it takes effect
/// as the in-flight ones finish.
///
/// An un-started receiver is not an error: there is no gate to resize, and the
/// value is already recorded for the start that eventually happens.
pub async fn set_sync_max_concurrent_receives(
    ctx: &ServiceContext,
    sync: &SyncRuntime,
    n: usize,
) -> Result<(), ApiError> {
    validate_max_concurrent_receives(n)?;
    // Scoped so the connection guard is dropped before the `inbound_control` await.
    {
        let db = db(ctx)?;
        let conn = db.conn();
        ctx.settings
            .persist_setting(&conn, keys::SYNC_MAX_CONCURRENT_RECEIVES, &n.to_string())
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    let applied_live = if let Some(control) = sync.inbound_control().await {
        control.receive_gate.set_limit(n);
        true
    } else {
        false
    };
    tracing::info!(
        max_concurrent_receives = n,
        applied_live,
        "sync receive concurrency cap set"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal real-`Database` [`ServiceContext`] (tempdir SQLite, no keychain
    /// involved anywhere) for exercising the settings-backed sync caches
    /// directly. Mirrors the construction pattern in `api::masters` tests.
    /// Seed one inbound row in `state` in the ctx's own sync store, returning its
    /// wire package id. The D2 cancel/delete tests need a durable row without a
    /// running receiver.
    fn seed_inbound(
        ctx: &ServiceContext,
        batch: &str,
        wire: &str,
        state: crate::sync::InboundState,
        reason: Option<&str>,
    ) -> i64 {
        use crate::sync::store::{set_inbound_state, upsert_inbound_attempt, CatalogSyncStore};
        let (_sync_dir, db_path) = sync_paths(ctx).unwrap();
        let store = CatalogSyncStore::open(&db_path).unwrap();
        let conn = store.lock_conn();
        let (id, _) = upsert_inbound_attempt(&conn, &"ee".repeat(32), batch, wire, 2, 100).unwrap();
        set_inbound_state(&conn, wire, state, reason).unwrap();
        id
    }

    fn read_inbound(ctx: &ServiceContext, wire: &str) -> crate::sync::models::InboundRow {
        use crate::sync::store::{get_inbound, CatalogSyncStore};
        let (_sync_dir, db_path) = sync_paths(ctx).unwrap();
        let store = CatalogSyncStore::open(&db_path).unwrap();
        let conn = store.lock_conn();
        get_inbound(&conn, wire).unwrap().unwrap()
    }

    /// D2 §4: declining a `Waiting` row must close it THERE AND THEN. There is no
    /// live fetch whose epilogue would stamp it and no announce is guaranteed to
    /// ever arrive, so without an explicit arm in the `stamp_now` match the row
    /// keeps `declined_at` and sits `waiting` forever — permanently undeletable,
    /// since `delete_transfer_history` refuses a non-terminal row.
    #[tokio::test]
    async fn cancelling_a_waiting_inbound_row_terminalizes_it_immediately() {
        let (_tmp, ctx) = test_ctx();
        let sync = SyncRuntime::default();
        seed_inbound(
            &ctx,
            "batch-1",
            "wire-1",
            crate::sync::InboundState::Waiting,
            Some("peer gone"),
        );

        cancel_incoming_package(&ctx, &sync, "wire-1")
            .await
            .unwrap();

        let row = read_inbound(&ctx, "wire-1");
        assert_eq!(
            row.state,
            crate::sync::InboundState::Cancelled,
            "closed on the spot — nothing else would ever close it"
        );
        assert!(row.declined_at.is_some(), "and the decline is final");
        assert!(row.finished_at.is_some(), "a terminal stamps finished_at");
    }

    /// D2 §4, the other side of the same user story: history deletion refuses a
    /// parked row, deliberately and symmetrically with the sent side. Cancel first,
    /// then delete — which is why the Cancel affordance is not optional.
    #[tokio::test]
    async fn delete_transfer_history_refuses_a_waiting_received_batch() {
        let (_tmp, ctx) = test_ctx();
        let sync = SyncRuntime::default();
        seed_inbound(
            &ctx,
            "batch-1",
            "wire-1",
            crate::sync::InboundState::Waiting,
            Some("peer gone"),
        );

        let err = delete_transfer_history(&ctx, &sync, Direction::Received, "batch-1".to_string())
            .await
            .expect_err("a non-terminal row refuses deletion");
        let text = format!("{err:?}").to_lowercase();
        assert!(
            text.contains("active") || text.contains("progress"),
            "the refusal names the live attempt: {err:?}"
        );

        let row = read_inbound(&ctx, "wire-1");
        assert_eq!(
            row.state,
            crate::sync::InboundState::Waiting,
            "and the row survives the refusal untouched"
        );
    }

    /// D1 §3.2 gate 1: a presence beacon must never make us ALLOCATE for a peer
    /// outside the account. This is a remote message, so the gate is checked from
    /// local state only — no hub round-trip, nothing to stall on.
    #[tokio::test]
    async fn presence_from_an_unauthorized_peer_builds_no_engine() {
        let (_tmp, ctx) = test_ctx();
        let ctx = Arc::new(ctx);
        {
            let conn = db(&ctx).unwrap().conn();
            // One authorized device — and the beacon below is NOT it.
            crate::db::set_setting(&conn, keys::SYNC_AUTHORIZED_PEERS, &"aa".repeat(32)).unwrap();
        }
        let sender = Arc::new(SyncSenderRuntime::new());
        let sync = Arc::new(SyncRuntime::default());
        let stranger: NodeId = [0x77; 32];

        handle_peer_presence(&ctx, &sender, &sender, &sync, None, stranger).await;

        assert!(
            sender.started_peers().await.is_empty(),
            "a stranger's beacon must not build an engine"
        );
        // The observable that proves the GATE stopped it, rather than the engine
        // build merely failing for want of a bound transport: the debounce is
        // stamped only after both gates pass, so an untouched stamp means the
        // handler returned before ever getting there.
        assert!(
            presence_debounce_admits(stranger),
            "a refused beacon must not have reached the debounce stamp"
        );
    }

    /// D1 §3.2 gate 2: an authorized peer with nothing pending is a no-op too —
    /// presence is a RESUME signal, not an engine-construction trigger.
    #[tokio::test]
    async fn presence_with_no_pending_rows_builds_no_engine() {
        let (_tmp, ctx) = test_ctx();
        let ctx = Arc::new(ctx);
        let peer: NodeId = [0xbb; 32];
        {
            let conn = db(&ctx).unwrap().conn();
            crate::db::set_setting(&conn, keys::SYNC_AUTHORIZED_PEERS, &node_id_hex(&peer))
                .unwrap();
        }
        let sender = Arc::new(SyncSenderRuntime::new());
        let sync = Arc::new(SyncRuntime::default());

        handle_peer_presence(&ctx, &sender, &sender, &sync, None, peer).await;

        assert!(
            sender.started_peers().await.is_empty(),
            "an authorized peer with no non-terminal rows has nothing to resume"
        );
        assert!(
            presence_debounce_admits(peer),
            "the pending-rows gate must stop the beacon before the debounce stamp"
        );
    }

    /// The debounce is per peer and bounded: a flapping relay on the peer's side
    /// must not turn one restart into a burst of resumes.
    #[test]
    fn presence_debounce_admits_once_per_window_per_peer() {
        let a: NodeId = [0x01; 32];
        let b: NodeId = [0x02; 32];
        assert!(
            presence_debounce_admits(a),
            "first beacon from a peer is admitted"
        );
        assert!(
            !presence_debounce_admits(a),
            "a second beacon inside the window is not"
        );
        assert!(
            presence_debounce_admits(b),
            "the window is per peer — another device is unaffected"
        );
    }

    fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
        use crate::cache::MemoryImageCache;
        use crate::services::compute_queue::ComputeQueue;
        use crate::services::operation_queue::OperationQueue;
        use crate::settings::SettingsManager;
        use std::collections::HashMap;
        #[cfg(all(feature = "render", feature = "solver"))]
        use std::sync::RwLock;
        use std::sync::{Mutex, OnceLock};

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
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        };
        (tmp, ctx)
    }

    /// tv2 UX wave 2: deleting a cancelled SENT batch removes every durable record
    /// for that batch — both attempt rows (sharing a dir basename), their per-file
    /// rows, their event journals, and their history rows — while a sibling batch
    /// is left completely intact.
    #[tokio::test]
    async fn delete_sent_batch_removes_all_records_and_leaves_siblings() {
        use crate::sharing::types::AnnounceFileEntry;
        use crate::sync::store::{
            append_sync_event, insert_history_row, insert_outbound_with_files,
        };

        let (_tmp, ctx) = test_ctx();
        let peer = "aa".repeat(32);
        let hist = |uuid: &str, key: &str| HistoryRow {
            frame_uuid: uuid.to_string(),
            filename: format!("{uuid}.fits"),
            object: Some("M42".into()),
            peer_device: peer.clone(),
            direction: Direction::Sent,
            bytes: 10,
            started_at: "2026-07-20T00:00:00Z".into(),
            finished_at: Some("2026-07-20T00:01:00Z".into()),
            outcome: "sent".into(),
            project: None,
            package_id: Some(key.to_string()),
            batch_name: None,
        };

        // Seed batchA as TWO cancelled attempts sharing the dir basename, each with
        // a per-file row + an event + a history row; plus a control batchB (one
        // cancelled attempt) that must survive untouched.
        let (a1, a2);
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let files = |uuid: &str| {
                vec![AnnounceFileEntry {
                    rel_path: format!("Lights/{uuid}.fits"),
                    byte_size: 10,
                    frame_uuid: uuid.to_string(),
                }]
            };
            a1 = insert_outbound_with_files(
                &conn,
                "/pkgs/batchA",
                &peer,
                Some("Batch A"),
                &files("f1"),
                PackageLayout::Batch,
            )
            .unwrap();
            a2 = insert_outbound_with_files(
                &conn,
                "/pkgs/batchA",
                &peer,
                Some("Batch A"),
                &files("f2"),
                PackageLayout::Batch,
            )
            .unwrap();
            let b1 = insert_outbound_with_files(
                &conn,
                "/pkgs/batchB",
                &peer,
                Some("Batch B"),
                &files("g1"),
                PackageLayout::Batch,
            )
            .unwrap();
            for id in [a1, a2, b1] {
                conn.execute(
                    "UPDATE sync_outbound SET state = 'cancelled' WHERE id = ?1",
                    [id],
                )
                .unwrap();
                append_sync_event(&conn, Direction::Sent, &id.to_string(), "cancel", None).unwrap();
            }
            insert_history_row(&conn, &hist("f1", "batchA")).unwrap();
            insert_history_row(&conn, &hist("f2", "batchA")).unwrap();
            insert_history_row(&conn, &hist("g1", "batchB")).unwrap();
        }

        let deleted = delete_transfer_history(
            &ctx,
            &SyncRuntime::new(),
            Direction::Sent,
            "batchA".to_string(),
        )
        .await
        .expect("delete ok");
        assert_eq!(deleted.rows, 2, "both batchA attempt rows deleted");
        assert_eq!(deleted.history, 2, "both batchA history rows deleted");

        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        // batchA fully gone across all four record tables.
        assert_eq!(
            count("SELECT COUNT(*) FROM sync_outbound WHERE package_ref = '/pkgs/batchA'"),
            0
        );
        assert_eq!(
            count(&format!(
                "SELECT COUNT(*) FROM sync_outbound_files WHERE outbound_id IN ({a1},{a2})"
            )),
            0
        );
        assert_eq!(
            count(&format!(
                "SELECT COUNT(*) FROM sync_events WHERE batch_key IN ('{a1}','{a2}')"
            )),
            0
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM sync_history WHERE package_id = 'batchA'"),
            0
        );
        // batchB intact — exactly its one file row, event, history row, and outbound row.
        assert_eq!(
            count("SELECT COUNT(*) FROM sync_outbound WHERE package_ref = '/pkgs/batchB'"),
            1
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM sync_outbound_files"),
            1,
            "only batchB's file row remains"
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM sync_events"),
            1,
            "only batchB's event remains"
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM sync_history WHERE package_id = 'batchB'"),
            1
        );
    }

    /// tv2 UX wave 2: a batch with ANY non-terminal attempt refuses deletion
    /// (`Invalid`) and removes nothing — the terminal guard is atomic with the
    /// deletes, so a mixed terminal+active batch stays completely intact.
    #[tokio::test]
    async fn delete_batch_refuses_when_an_attempt_is_active() {
        use crate::sharing::types::AnnounceFileEntry;
        use crate::sync::store::{insert_history_row, insert_outbound_with_files};

        let (_tmp, ctx) = test_ctx();
        let peer = "bb".repeat(32);
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let files = vec![AnnounceFileEntry {
                rel_path: "Lights/x.fits".into(),
                byte_size: 4,
                frame_uuid: "x".into(),
            }];
            // Attempt 1 cancelled (terminal); attempt 2 transferring (non-terminal).
            let done = insert_outbound_with_files(
                &conn,
                "/pkgs/live",
                &peer,
                None,
                &files,
                PackageLayout::Batch,
            )
            .unwrap();
            conn.execute(
                "UPDATE sync_outbound SET state = 'cancelled' WHERE id = ?1",
                [done],
            )
            .unwrap();
            let live = insert_outbound_with_files(
                &conn,
                "/pkgs/live",
                &peer,
                None,
                &files,
                PackageLayout::Batch,
            )
            .unwrap();
            conn.execute(
                "UPDATE sync_outbound SET state = 'transferring' WHERE id = ?1",
                [live],
            )
            .unwrap();
            insert_history_row(
                &conn,
                &HistoryRow {
                    frame_uuid: "x".into(),
                    filename: "x.fits".into(),
                    object: None,
                    peer_device: peer.clone(),
                    direction: Direction::Sent,
                    bytes: 4,
                    started_at: "t".into(),
                    finished_at: None,
                    outcome: "sent".into(),
                    project: None,
                    package_id: Some("live".into()),
                    batch_name: None,
                },
            )
            .unwrap();
        }

        let err = delete_transfer_history(
            &ctx,
            &SyncRuntime::new(),
            Direction::Sent,
            "live".to_string(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, ApiError::Invalid(_)),
            "active attempt refuses with Invalid"
        );

        // Nothing deleted — both rows and the history row survive.
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_outbound WHERE package_ref = '/pkgs/live'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2, "both attempt rows still present");
        let h: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_history WHERE package_id = 'live'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(h, 1, "history untouched");
    }

    /// tv2 UX wave 2: deleting a cancelled RECEIVED batch (keyed by wire package
    /// uuid) removes the inbound row, its per-file rows, its event journal, and its
    /// received history rows.
    #[tokio::test]
    async fn delete_received_batch_removes_all_records() {
        use crate::sync::store::{
            append_sync_event, insert_history_row, replace_inbound_files, set_inbound_state,
            upsert_inbound_announced,
        };
        use crate::sync::{InboundFileRow, InboundFileState, InboundState};

        let (_tmp, ctx) = test_ctx();
        let peer = "cc".repeat(32);
        let pkg = "pkg-uuid-1";
        let id;
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            id = upsert_inbound_announced(&conn, &peer, pkg, 1, 10).unwrap();
            replace_inbound_files(
                &conn,
                id,
                &[InboundFileRow {
                    inbound_id: id,
                    rel_path: "Lights/r.fits".into(),
                    byte_size: 10,
                    frame_uuid: "r".into(),
                    state: InboundFileState::Done,
                    bytes_done: 10,
                    outcome: Some("ingested".into()),
                    error: None,
                    updated_at: "t".into(),
                }],
            )
            .unwrap();
            append_sync_event(
                &conn,
                Direction::Received,
                &id.to_string(),
                "fetch_done",
                None,
            )
            .unwrap();
            set_inbound_state(&conn, pkg, InboundState::Cancelled, None).unwrap();
            insert_history_row(
                &conn,
                &HistoryRow {
                    frame_uuid: "r".into(),
                    filename: "r.fits".into(),
                    object: None,
                    peer_device: peer.clone(),
                    direction: Direction::Received,
                    bytes: 10,
                    started_at: "t".into(),
                    finished_at: Some("t2".into()),
                    outcome: "cancelled".into(),
                    project: None,
                    package_id: Some(pkg.into()),
                    batch_name: None,
                },
            )
            .unwrap();
        }

        let deleted = delete_transfer_history(
            &ctx,
            &SyncRuntime::new(),
            Direction::Received,
            pkg.to_string(),
        )
        .await
        .expect("delete ok");
        assert_eq!(deleted.rows, 1, "the inbound row deleted");
        assert_eq!(deleted.history, 1, "the received history row deleted");

        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM sync_inbound"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM sync_inbound_files"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM sync_events"), 0);
        assert_eq!(
            count("SELECT COUNT(*) FROM sync_history WHERE direction = 'received'"),
            0
        );
    }

    /// B5b: a RECEIVED batch deletes by its durable `batch_uuid` (not the rotating
    /// wire id) — the symmetric-with-sent key. A modern batch-keyed row whose current
    /// wire id differs from the batch_uuid is removed along with its files, events,
    /// its batch-keyed history rows AND any wire-id-keyed history left from before
    /// B5b (the current-wire-id sweep). Its blob tags (the CURRENT wire id) are the
    /// reclaim targets.
    #[tokio::test]
    async fn delete_received_batch_by_batch_uuid_removes_all_attempts_history() {
        use crate::sync::store::{
            append_sync_event, insert_history_row, replace_inbound_files, set_inbound_state,
            upsert_inbound_attempt,
        };
        use crate::sync::{InboundFileRow, InboundFileState, InboundState};

        let (_tmp, ctx) = test_ctx();
        let peer = "cc".repeat(32);
        const BATCH: &str = "batch-recv-1";
        let wire = "wire-current-2"; // current attempt's wire id — differs from BATCH
        let id;
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            // A modern batch-keyed inbound row (batch_uuid = BATCH, wire id = wire).
            let (rid, _) = upsert_inbound_attempt(&conn, &peer, BATCH, wire, 1, 10).unwrap();
            id = rid;
            assert_ne!(
                wire, BATCH,
                "wire id must differ from the batch_uuid for this test"
            );
            replace_inbound_files(
                &conn,
                id,
                &[InboundFileRow {
                    inbound_id: id,
                    rel_path: "Lights/r.fits".into(),
                    byte_size: 10,
                    frame_uuid: "r".into(),
                    state: InboundFileState::Done,
                    bytes_done: 10,
                    outcome: Some("ingested".into()),
                    error: None,
                    updated_at: "t".into(),
                }],
            )
            .unwrap();
            append_sync_event(
                &conn,
                Direction::Received,
                &id.to_string(),
                "fetch_done",
                None,
            )
            .unwrap();
            set_inbound_state(&conn, wire, InboundState::Done, None).unwrap();
            // The batch-keyed history row (B5b writers).
            insert_history_row(
                &conn,
                &HistoryRow {
                    frame_uuid: "r".into(),
                    filename: "r.fits".into(),
                    object: None,
                    peer_device: peer.clone(),
                    direction: Direction::Received,
                    bytes: 10,
                    started_at: "t".into(),
                    finished_at: Some("t2".into()),
                    outcome: "ingested".into(),
                    project: None,
                    package_id: Some(BATCH.into()),
                    batch_name: None,
                },
            )
            .unwrap();
            // A pre-B5b wire-id-keyed history row (same dev cycle) — swept by the
            // current-wire-id second delete.
            insert_history_row(
                &conn,
                &HistoryRow {
                    frame_uuid: "r-old".into(),
                    filename: "r-old.fits".into(),
                    object: None,
                    peer_device: peer.clone(),
                    direction: Direction::Received,
                    bytes: 10,
                    started_at: "t".into(),
                    finished_at: Some("t2".into()),
                    outcome: "ingested".into(),
                    project: None,
                    package_id: Some(wire.into()),
                    batch_name: None,
                },
            )
            .unwrap();
        }

        let deleted = delete_transfer_history(
            &ctx,
            &SyncRuntime::new(),
            Direction::Received,
            BATCH.to_string(),
        )
        .await
        .expect("delete ok");
        assert_eq!(
            deleted.rows, 1,
            "the one batch row deleted (matched by batch_uuid)"
        );
        assert_eq!(
            deleted.history, 2,
            "both the batch-keyed and the current-wire-id-keyed history rows deleted"
        );

        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM sync_inbound"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM sync_inbound_files"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM sync_events"), 0);
        assert_eq!(
            count("SELECT COUNT(*) FROM sync_history WHERE direction = 'received'"),
            0
        );
    }

    /// B5b: a terminal received row surfaces its `last_error` on the summary — so a
    /// sender-revoked transfer can honestly say WHY it ended (e.g. «by sender»)
    /// instead of a bare `cancelled`. The revoke path already writes the reason via
    /// `set_inbound_state`; this pins the summary mapper exposing it.
    #[tokio::test]
    async fn inbound_summary_exposes_last_error() {
        use crate::sync::store::{set_inbound_state, upsert_inbound_announced};
        use crate::sync::InboundState;

        let (_tmp, ctx) = test_ctx();
        let peer = "bb".repeat(32);
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let _ = upsert_inbound_announced(&conn, &peer, "wire-rev", 1, 10).unwrap();
            // What `handle_revoke(Cancelled)` stamps: state cancelled + "by sender".
            set_inbound_state(
                &conn,
                "wire-rev",
                InboundState::Cancelled,
                Some("by sender"),
            )
            .unwrap();
        }

        let terminal = list_terminal_transfers(&ctx, None).expect("terminal transfers");
        let recv = terminal
            .received
            .iter()
            .find(|r| r.package_id == "wire-rev")
            .expect("received summary present");
        assert_eq!(recv.state, InboundState::Cancelled);
        assert_eq!(
            recv.last_error.as_deref(),
            Some("by sender"),
            "the sender-revoke reason surfaces on the inbound summary"
        );
    }

    /// D2 §3.6: the two sides meet in one chip. A parked received row renders as
    /// `waiting_peer` — the same label, subline and filter bucket D1 gave the send
    /// side — even though the mechanics beneath differ (stored here, derived
    /// there). Every other state still echoes raw: the mapping is one arm, not a
    /// rewrite.
    #[tokio::test]
    async fn a_waiting_inbound_row_displays_as_waiting_peer() {
        use crate::sync::store::{set_inbound_state, upsert_inbound_announced, CatalogSyncStore};
        use crate::sync::InboundState;

        let (_tmp, ctx) = test_ctx();
        let (_sync_dir, db_path) = sync_paths(&ctx).unwrap();
        let peer = "cc".repeat(32);
        {
            let store = CatalogSyncStore::open(&db_path).unwrap();
            let conn = store.lock_conn();
            upsert_inbound_announced(&conn, &peer, "wire-wait", 2, 100).unwrap();
            set_inbound_state(
                &conn,
                "wire-wait",
                InboundState::Waiting,
                Some("peer gone: connection lost"),
            )
            .unwrap();
            // A sibling still fetching, to prove the mapping is one arm.
            upsert_inbound_announced(&conn, &peer, "wire-live", 2, 100).unwrap();
            set_inbound_state(&conn, "wire-live", InboundState::Fetching, None).unwrap();
        }

        let active =
            active_inbound_summaries(&ctx, &HashSet::new()).expect("active inbound summaries");
        let parked = active
            .iter()
            .find(|r| r.package_id == "wire-wait")
            .expect("the parked row is ACTIVE — that is the point of non-terminality");
        assert_eq!(
            parked.display_state, "waiting_peer",
            "one chip for both directions"
        );
        assert_eq!(
            parked.state,
            InboundState::Waiting,
            "the raw state still ships alongside for the frontend's own gating"
        );
        assert_eq!(
            parked.last_error.as_deref(),
            Some("peer gone: connection lost"),
            "and the reason is readable in Details"
        );

        let live = active
            .iter()
            .find(|r| r.package_id == "wire-live")
            .expect("the fetching row");
        assert_eq!(
            live.display_state, "fetching",
            "every other state still echoes raw"
        );
    }

    /// Perseus UI v2 (Task 10): the inbound mapper resolves `peer_kind` with a
    /// stamp-wins-over-cache precedence — the value stamped on the row at announce
    /// time (Task 9) wins; a legacy row whose stamp is NULL falls back to the
    /// cached `SYNC_PEER_CAPABILITIES` hex→kind map keyed on the sending peer;
    /// neither present → `None`.
    #[test]
    fn inbound_summary_resolves_peer_kind_stamp_then_cache_then_none() {
        use crate::sync::InboundState;

        let peer = "cc".repeat(32);
        let row = |peer_capability: Option<&str>| InboundRow {
            id: 1,
            peer: peer.clone(),
            package_id: "wire-1".into(),
            state: InboundState::Done,
            frame_count: 1,
            byte_size: 10,
            bytes_done: 10,
            last_error: None,
            created_at: "2026-07-22T00:00:00Z".into(),
            finished_at: None,
            display_name: None,
            landing_dir: None,
            batch_uuid: Some("batch-1".into()),
            project_id: None,
            generation: 1,
            declined_at: None,
            peer_capability: peer_capability.map(str::to_string),
        };
        let names: HashMap<String, String> = HashMap::new();
        let counts: HashMap<i64, TransferFileCounts> = HashMap::new();

        // 1) A stamped row wins even when the cache says otherwise.
        let mut kinds = HashMap::new();
        kinds.insert(peer.clone(), "athenaeum".to_string());
        let s = inbound_summary(
            row(Some("perseus")),
            &names,
            &kinds,
            &counts,
            &HashSet::new(),
        );
        assert_eq!(
            s.peer_kind.as_deref(),
            Some("perseus"),
            "stamped capability wins over cache"
        );

        // 2) A NULL-stamped row + a cache hit → the cache value.
        let s = inbound_summary(row(None), &names, &kinds, &counts, &HashSet::new());
        assert_eq!(
            s.peer_kind.as_deref(),
            Some("athenaeum"),
            "NULL stamp falls back to the cache"
        );

        // 3) Neither a stamp nor a cache entry → None.
        let empty: HashMap<String, String> = HashMap::new();
        let s = inbound_summary(row(None), &names, &empty, &counts, &HashSet::new());
        assert_eq!(s.peer_kind, None, "neither stamp nor cache → None");
    }

    /// Variant B at the command boundary: the status poll surfaces the announces
    /// queued on a busy peer's lane — and NEVER one that already has a live row.
    ///
    /// The sender re-announces on every backoff rung while it waits for an ack, so
    /// the batch currently being fetched keeps re-queueing on its own lane behind
    /// itself. Surfacing that would put a ghost next to the live row for the SAME
    /// transfer, under the same `batchUuid` the Transfers UI keys rows on. The
    /// genuinely-queued sibling — no row anywhere — is the one that must show up.
    #[tokio::test]
    async fn get_status_surfaces_queued_inbound_and_drops_row_duplicates() {
        use crate::sync::store::{upsert_inbound_attempt, CatalogSyncStore};

        let (tmp, ctx) = test_ctx();
        let (_ep, runtime) = started_loopback_runtime(&tmp).await;
        let sender = SyncSenderRuntime::new();

        let peer_bytes: NodeId = [0xcc; 32];
        let peer = node_id_hex(&peer_bytes);

        // The transfer this device is fetching right now: a live, non-terminal row.
        let (_sync_dir, db_path) = sync_paths(&ctx).unwrap();
        {
            let store = CatalogSyncStore::open(&db_path).unwrap();
            let conn = store.lock_conn();
            upsert_inbound_attempt(&conn, &peer, "batch-live", "wire-live", 2, 100).unwrap();
        }

        let control = runtime
            .inbound_control()
            .await
            .expect("a started runtime exposes its inbound control");
        // Its own re-announce, queued behind itself on the peer's lane…
        control.note_queued_announce(peer_bytes, "batch-live", Some("Live Batch".into()), 2, 100);
        // …and the sibling that genuinely has nowhere else to be seen.
        control.note_queued_announce(peer_bytes, "batch-next", Some("Next Batch".into()), 5, 500);

        let status = get_status(&ctx, &runtime, &sender).await.unwrap();

        assert!(
            status
                .receiver
                .active
                .iter()
                .any(|r| r.batch_uuid == "batch-live"),
            "the live row is where it always was"
        );
        assert_eq!(
            status.receiver.queued.len(),
            1,
            "the re-announce of the batch that already has a row is dropped, got: {:?}",
            status
                .receiver
                .queued
                .iter()
                .map(|q| q.batch_uuid.as_str())
                .collect::<Vec<_>>()
        );
        let ghost = &status.receiver.queued[0];
        assert_eq!(ghost.batch_uuid, "batch-next");
        assert_eq!(ghost.batch_name.as_deref(), Some("Next Batch"));
        assert_eq!(ghost.frame_count, 5);
        assert_eq!(ghost.byte_size, 500);
        assert_eq!(
            ghost.peer_short,
            short_id(&peer),
            "the ghost carries the same short peer handle a real row would"
        );
        assert_eq!(
            ghost.device_name, None,
            "no cached device name for this peer — resolution is attempted, not invented"
        );

        // No receiver started ⇒ no lane queue to report on. Not an error, just empty.
        let unstarted = SyncRuntime::new();
        let status = get_status(&ctx, &unstarted, &sender).await.unwrap();
        assert!(
            status.receiver.queued.is_empty(),
            "an unstarted receiver has no queued announces"
        );
    }

    /// Variant C at the command boundary: a row whose lane is parked on the receive
    /// gate reads `queued`, not `announced`.
    ///
    /// Both rows are `Announced` in the DB — that is the whole problem the live
    /// parked set exists to solve. Durable state cannot tell "this just arrived" from
    /// "this is waiting for a free receive slot", so a transfer blocked purely on the
    /// concurrency cap showed a bare "announced" with nothing to suggest it starts by
    /// itself.
    ///
    /// Three rows, one signal: the parked one relabels, the plain announced one does
    /// NOT, and a parked id whose row has since moved to `Fetching` also does not —
    /// the arm is gated on `Announced` alone, so a stale entry (the set and the rows
    /// are read a moment apart) can never mislabel a running transfer.
    #[tokio::test]
    async fn a_parked_inbound_row_displays_queued() {
        use crate::sync::store::{set_inbound_state, upsert_inbound_attempt, CatalogSyncStore};
        use crate::sync::InboundState;

        let (tmp, ctx) = test_ctx();
        let (_ep, runtime) = started_loopback_runtime(&tmp).await;
        let sender = SyncSenderRuntime::new();

        let peer = "cc".repeat(32);
        let (_sync_dir, db_path) = sync_paths(&ctx).unwrap();
        {
            let store = CatalogSyncStore::open(&db_path).unwrap();
            let conn = store.lock_conn();
            // Parked on the gate, and one that simply arrived — indistinguishable here.
            upsert_inbound_attempt(&conn, &peer, "batch-parked", "wire-parked", 2, 100).unwrap();
            upsert_inbound_attempt(&conn, &peer, "batch-fresh", "wire-fresh", 2, 100).unwrap();
            // Admitted and running, but still in the parked set (the stale-entry case).
            upsert_inbound_attempt(&conn, &peer, "batch-moved", "wire-moved", 2, 100).unwrap();
            set_inbound_state(&conn, "wire-moved", InboundState::Fetching, None).unwrap();
        }

        let control = runtime
            .inbound_control()
            .await
            .expect("a started runtime exposes its inbound control");
        // Exactly what `handle_announce`'s guard records: the WIRE package id.
        control.note_parked_for_slot("wire-parked");
        control.note_parked_for_slot("wire-moved");

        let status = get_status(&ctx, &runtime, &sender).await.unwrap();
        let row = |wire: &str| {
            status
                .receiver
                .active
                .iter()
                .find(|r| r.package_id == wire)
                .unwrap_or_else(|| panic!("{wire} is a non-terminal row — it must be ACTIVE"))
        };

        let parked = row("wire-parked");
        assert_eq!(
            parked.display_state, "queued",
            "a transfer waiting for a receive slot says so"
        );
        assert_eq!(
            parked.state,
            InboundState::Announced,
            "the raw state is untouched — `queued` is a display label over `announced`, \
             and the frontend still gates on the raw state"
        );

        assert_eq!(
            row("wire-fresh").display_state,
            "announced",
            "a row nobody parked still echoes raw"
        );
        assert_eq!(
            row("wire-moved").display_state,
            "fetching",
            "the relabel is gated on `Announced` — a stale parked entry never overrides \
             a transfer that has already been admitted"
        );

        // No receiver started ⇒ no parked set ⇒ every row reads raw. Not an error.
        let unstarted = SyncRuntime::new();
        let status = get_status(&ctx, &unstarted, &sender).await.unwrap();
        assert!(
            status
                .receiver
                .active
                .iter()
                .all(|r| r.display_state != "queued"),
            "an unstarted receiver has nothing parked"
        );
    }

    /// Dead-peer delete (tv2 blocker fix): a SENT batch whose only non-terminal
    /// attempt is a permanent orphan — an old `transferring` row whose peer is no
    /// longer in the account's cached device set — must delete cleanly. Such a row
    /// has no engine (the resurrection account-gate skips it), is invisible in the
    /// active list, and can never be cancelled, so the old terminal-only guard
    /// blocked record deletion forever. With a NON-EMPTY device cache that does
    /// NOT list the peer, the orphan is cancelled in-place and the whole batch
    /// (both attempt rows, their files, events, and history) is removed.
    #[tokio::test]
    async fn delete_sent_batch_cancels_and_removes_dead_peer_orphan() {
        use crate::sharing::types::AnnounceFileEntry;
        use crate::sync::store::{
            append_sync_event, insert_history_row, insert_outbound_with_files,
        };

        let (_tmp, ctx) = test_ctx();
        let dead_peer = "de".repeat(32); // absent from the seeded account cache
        let live_peer = "aa".repeat(32); // the sole seeded account device
        let (term, orphan);
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            // Non-empty account-device cache that does NOT contain `dead_peer`.
            crate::db::set_setting(&conn, keys::SYNC_AUTHORIZED_PEERS, &live_peer).unwrap();
            let files = |u: &str| {
                vec![AnnounceFileEntry {
                    rel_path: format!("Lights/{u}.fits"),
                    byte_size: 8,
                    frame_uuid: u.to_string(),
                }]
            };
            // Terminal sibling + a non-terminal orphan sharing the dir basename.
            term = insert_outbound_with_files(
                &conn,
                "/pkgs/gone",
                &dead_peer,
                None,
                &files("f1"),
                PackageLayout::Batch,
            )
            .unwrap();
            orphan = insert_outbound_with_files(
                &conn,
                "/pkgs/gone",
                &dead_peer,
                None,
                &files("f2"),
                PackageLayout::Batch,
            )
            .unwrap();
            conn.execute(
                "UPDATE sync_outbound SET state = 'cancelled' WHERE id = ?1",
                [term],
            )
            .unwrap();
            conn.execute(
                "UPDATE sync_outbound SET state = 'transferring' WHERE id = ?1",
                [orphan],
            )
            .unwrap();
            append_sync_event(&conn, Direction::Sent, &term.to_string(), "cancel", None).unwrap();
            append_sync_event(&conn, Direction::Sent, &orphan.to_string(), "attempt", None)
                .unwrap();
            insert_history_row(
                &conn,
                &HistoryRow {
                    frame_uuid: "f2".into(),
                    filename: "f2.fits".into(),
                    object: None,
                    peer_device: dead_peer.clone(),
                    direction: Direction::Sent,
                    bytes: 8,
                    started_at: "t".into(),
                    finished_at: None,
                    outcome: "sent".into(),
                    project: None,
                    package_id: Some("gone".into()),
                    batch_name: None,
                },
            )
            .unwrap();
        }

        let deleted = delete_transfer_history(
            &ctx,
            &SyncRuntime::new(),
            Direction::Sent,
            "gone".to_string(),
        )
        .await
        .expect("dead-peer orphan deletes");
        assert_eq!(deleted.rows, 2, "both attempt rows deleted");
        assert_eq!(deleted.history, 1, "the history row deleted");

        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(
            count("SELECT COUNT(*) FROM sync_outbound WHERE package_ref = '/pkgs/gone'"),
            0
        );
        assert_eq!(
            count(&format!(
                "SELECT COUNT(*) FROM sync_outbound_files WHERE outbound_id IN ({term},{orphan})"
            )),
            0
        );
        assert_eq!(
            count(&format!(
                "SELECT COUNT(*) FROM sync_events WHERE batch_key IN ('{term}','{orphan}')"
            )),
            0
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM sync_history WHERE package_id = 'gone'"),
            0
        );
    }

    /// Dead-peer delete guard: a non-terminal attempt whose peer IS still a live
    /// account device is genuinely in flight — deletion must refuse (deleting
    /// nothing), and the message must name the state so the UI can point the user
    /// at the live row to cancel it there.
    #[tokio::test]
    async fn delete_sent_batch_refuses_when_peer_still_in_account() {
        use crate::sharing::types::AnnounceFileEntry;
        use crate::sync::store::insert_outbound_with_files;

        let (_tmp, ctx) = test_ctx();
        let live_peer = "aa".repeat(32);
        let live;
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            crate::db::set_setting(&conn, keys::SYNC_AUTHORIZED_PEERS, &live_peer).unwrap();
            let files = vec![AnnounceFileEntry {
                rel_path: "Lights/x.fits".into(),
                byte_size: 4,
                frame_uuid: "x".into(),
            }];
            live = insert_outbound_with_files(
                &conn,
                "/pkgs/live",
                &live_peer,
                None,
                &files,
                PackageLayout::Batch,
            )
            .unwrap();
            conn.execute(
                "UPDATE sync_outbound SET state = 'transferring' WHERE id = ?1",
                [live],
            )
            .unwrap();
        }

        let err = delete_transfer_history(
            &ctx,
            &SyncRuntime::new(),
            Direction::Sent,
            "live".to_string(),
        )
        .await
        .unwrap_err();
        match err {
            ApiError::Invalid(msg) => assert!(
                msg.contains("transferring"),
                "refusal names the active state, got: {msg}"
            ),
            other => panic!("expected Invalid, got {other:?}"),
        }

        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_outbound WHERE package_ref = '/pkgs/live'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n, 1,
            "nothing deleted while the peer is still an account device"
        );
        let state: String = conn
            .query_row(
                "SELECT state FROM sync_outbound WHERE id = ?1",
                [live],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "transferring", "the live attempt is left untouched");
    }

    /// Dead-peer delete guard: with a COLD device cache (never populated), liveness
    /// is unknowable — the delete must refuse with the retry message rather than
    /// guess the peer left the account, and touch nothing.
    #[tokio::test]
    async fn delete_sent_batch_refuses_when_device_cache_cold() {
        use crate::sharing::types::AnnounceFileEntry;
        use crate::sync::store::insert_outbound_with_files;

        let (_tmp, ctx) = test_ctx();
        let peer = "de".repeat(32);
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            // Deliberately DO NOT seed SYNC_AUTHORIZED_PEERS — cold cache.
            let files = vec![AnnounceFileEntry {
                rel_path: "Lights/x.fits".into(),
                byte_size: 4,
                frame_uuid: "x".into(),
            }];
            let id = insert_outbound_with_files(
                &conn,
                "/pkgs/cold",
                &peer,
                None,
                &files,
                PackageLayout::Batch,
            )
            .unwrap();
            conn.execute(
                "UPDATE sync_outbound SET state = 'transferring' WHERE id = ?1",
                [id],
            )
            .unwrap();
        }

        let err = delete_transfer_history(
            &ctx,
            &SyncRuntime::new(),
            Direction::Sent,
            "cold".to_string(),
        )
        .await
        .unwrap_err();
        match err {
            ApiError::Invalid(msg) => assert!(
                msg.contains("device list unavailable"),
                "cold cache refuses with the retry message, got: {msg}"
            ),
            other => panic!("expected Invalid, got {other:?}"),
        }

        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_outbound WHERE package_ref = '/pkgs/cold'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n, 1,
            "cold cache never deletes — no guessing about liveness"
        );
    }

    /// Batch Model §D4 (delete = reclaim, sent): deleting a terminal SENT batch
    /// removes its on-disk payload directory (`package_ref`) after the records
    /// commit — best-effort, and the delete itself still succeeds. Uses a real
    /// tempdir payload so the reclaim's fs removal is observable.
    #[tokio::test]
    async fn delete_sent_batch_reclaims_payload_dir() {
        use crate::sharing::types::AnnounceFileEntry;
        use crate::sync::store::insert_outbound_with_files;

        let (tmp, ctx) = test_ctx();
        // A real payload dir whose basename == the batch key.
        let payload_dir = tmp.path().join("packages").join("batchZ");
        std::fs::create_dir_all(&payload_dir).unwrap();
        std::fs::write(payload_dir.join("frame.fits"), b"payload").unwrap();
        assert!(payload_dir.exists(), "payload dir seeded");

        let peer = "aa".repeat(32);
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let files = vec![AnnounceFileEntry {
                rel_path: "Lights/frame.fits".into(),
                byte_size: 7,
                frame_uuid: "frame".into(),
            }];
            let id = insert_outbound_with_files(
                &conn,
                payload_dir.to_str().unwrap(),
                &peer,
                Some("Batch Z"),
                &files,
                PackageLayout::Batch,
            )
            .unwrap();
            conn.execute(
                "UPDATE sync_outbound SET state = 'confirmed' WHERE id = ?1",
                [id],
            )
            .unwrap();
        }

        let deleted = delete_transfer_history(
            &ctx,
            &SyncRuntime::new(),
            Direction::Sent,
            "batchZ".to_string(),
        )
        .await
        .expect("delete + reclaim ok");
        assert_eq!(deleted.rows, 1, "the outbound row deleted");
        assert!(
            !payload_dir.exists(),
            "delete reclaims the sent batch's payload dir"
        );
    }

    /// Batch Model §D4 (delete = reclaim, received): deleting a terminal RECEIVED
    /// batch releases its blob tags through the live receive-side transport (the
    /// same `transport.release` the receiver's cancel/revoke paths use). Asserted on
    /// the loopback `served` map (its observable for a released tag).
    #[tokio::test]
    async fn delete_received_batch_releases_blob_tags() {
        use crate::sharing::loopback::LoopbackNetwork;
        use crate::sharing::types::PackageAnnounce;
        use crate::sharing::SharingTransport;
        use crate::sync::receiver::{
            allow_all_peers, InboundControl, IncomingResolver, SyncReceiver,
        };
        use crate::sync::store::{upsert_inbound_announced, CatalogSyncStore};
        use crate::sync::InboundState;

        let (tmp, ctx) = test_ctx();
        let wire = "wire-recv-reclaim";
        let peer = "bb".repeat(32);

        // Seed a TERMINAL inbound row (failed → an errored fetch retains its
        // in-flight tag) in the ctx catalog the delete reads.
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let id = upsert_inbound_announced(&conn, &peer, wire, 1, 10).unwrap();
            crate::sync::store::set_inbound_state(&conn, wire, InboundState::Failed, Some("boom"))
                .unwrap();
            let _ = id;
        }

        // A started receive-side runtime over a loopback endpoint that "serves"
        // the wire id (the loopback stand-in for the receiver's in-flight tag).
        let net = LoopbackNetwork::new();
        let ep = Arc::new(net.endpoint());
        let recv_store = Arc::new(CatalogSyncStore::open(tmp.path().join("recv.db")).unwrap());
        let staging = tmp.path().join("recv-staging");
        let incoming: IncomingResolver = {
            let p = tmp.path().join("recv-incoming");
            Arc::new(move || p.clone())
        };
        let (_info, handle) = SyncReceiver::spawn(
            recv_store,
            staging,
            incoming,
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&ep) as Arc<dyn SharingTransport>,
            Arc::new(crate::events::NullEmitter),
        )
        .await
        .unwrap();
        let runtime = SyncRuntime::new();
        runtime
            .set_started_for_test(
                Arc::clone(&ep) as Arc<dyn SharingTransport>,
                handle,
                "ticket".into(),
            )
            .await;

        let src = tmp.path().join("served-src");
        std::fs::create_dir_all(&src).unwrap();
        let announce = PackageAnnounce {
            package_id: crate::sharing::types::PackageId(wire.to_string()),
            root_hash: "0".repeat(64),
            byte_size: 10,
            frame_count: 1,
        };
        ep.serve(&announce, &src, None).await.unwrap();
        assert!(ep.is_serving(wire), "the wire id is served before delete");

        let deleted =
            delete_transfer_history(&ctx, &runtime, Direction::Received, wire.to_string())
                .await
                .expect("delete + reclaim ok");
        assert_eq!(deleted.rows, 1, "the inbound row deleted");
        assert!(
            !ep.is_serving(wire),
            "delete releases the received batch's blob tags"
        );
    }

    /// B7 helper: a started receive-side [`SyncRuntime`] over a loopback endpoint
    /// (the same substitution `delete_received_batch_releases_blob_tags` makes), so
    /// `sync.transport()` hands the loopback back to the sweep/cleanup in-flight-tag
    /// pass. Returns the endpoint (to seed/observe in-flight tags) + the runtime.
    async fn started_loopback_runtime(
        tmp: &tempfile::TempDir,
    ) -> (
        Arc<crate::sharing::loopback::LoopbackTransport>,
        Arc<SyncRuntime>,
    ) {
        use crate::sharing::loopback::LoopbackNetwork;
        use crate::sync::receiver::{
            allow_all_peers, InboundControl, IncomingResolver, SyncReceiver,
        };
        use crate::sync::store::CatalogSyncStore;

        let net = LoopbackNetwork::new();
        let ep = Arc::new(net.endpoint());
        let recv_store = Arc::new(CatalogSyncStore::open(tmp.path().join("recv.db")).unwrap());
        let staging = tmp.path().join("recv-staging");
        let incoming: IncomingResolver = {
            let p = tmp.path().join("recv-incoming");
            Arc::new(move || p.clone())
        };
        let (_info, handle) = SyncReceiver::spawn(
            recv_store,
            staging,
            incoming,
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&ep) as Arc<dyn SharingTransport>,
            Arc::new(crate::events::NullEmitter),
        )
        .await
        .unwrap();
        let runtime = Arc::new(SyncRuntime::new());
        runtime
            .set_started_for_test(
                Arc::clone(&ep) as Arc<dyn SharingTransport>,
                handle,
                "t".into(),
            )
            .await;
        (ep, runtime)
    }

    /// Test helper: force `path`'s OWN mtime (no recursion) to `age` in the past.
    /// Works on a directory via `File::open` + the stable `set_modified` (no extra
    /// crate needed).
    fn set_mtime_ago(path: &Path, age: Duration) {
        let f = std::fs::File::open(path)
            .unwrap_or_else(|e| panic!("open {} for mtime set: {e}", path.display()));
        f.set_modified(SystemTime::now() - age)
            .expect("set_modified");
    }

    /// Test helper: set `path` AND every entry recursively under it (files
    /// included) to `age` in the past, so a test can put a whole orphan-sweep
    /// candidate subtree on either side of [`ORPHAN_SWEEP_MIN_AGE`] without
    /// sleeping. Recursive because [`dir_age`] now reads the MAX mtime across the
    /// whole subtree (hardening fix) — backdating only the top-level dir would
    /// leave a just-created file's own (fresh) mtime dominating the max, so the
    /// dir would still read as young.
    fn backdate_mtime(path: &Path, age: Duration) {
        for entry in walkdir::WalkDir::new(path).into_iter().flatten() {
            set_mtime_ago(entry.path(), age);
        }
    }

    /// B7 core contract: the startup sweep removes ONLY row-less orphan payload
    /// dirs (a terminal-row dir and a non-terminal-row dir are both kept), and
    /// releases ONLY orphan in-flight tags (a tag whose wire id still has a
    /// non-terminal inbound row is kept).
    #[tokio::test]
    async fn sweep_removes_only_rowless_orphans_and_orphan_tags() {
        use crate::sharing::types::AnnounceFileEntry;
        use crate::sync::store::{insert_outbound_with_files, upsert_inbound_announced};

        let (tmp, ctx) = test_ctx();
        let packages_dir = sender_packages_dir(&ctx).unwrap();
        std::fs::create_dir_all(&packages_dir).unwrap();

        // Three payload dirs under <sync>/packages/, each with a payload file.
        let mk = |name: &str| {
            let d = packages_dir.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("frame.fits"), b"payload").unwrap();
            d
        };
        let orphan_dir = mk("orphan-uuid"); // (a) NO row → removed
                                            // Past the age gate: this dir must read as a genuine prior-session orphan,
                                            // not a concurrent enqueue's in-progress dir-before-row window.
        backdate_mtime(&orphan_dir, ORPHAN_SWEEP_MIN_AGE + Duration::from_secs(60));
        let terminal_dir = mk("terminal-uuid"); // (b) terminal row → kept
        let live_dir = mk("live-uuid"); // (c) non-terminal row → kept

        let peer = "aa".repeat(32);
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let files = vec![AnnounceFileEntry {
                rel_path: "L/frame.fits".into(),
                byte_size: 7,
                frame_uuid: "f".into(),
            }];
            // (b) terminal (confirmed) row referencing terminal_dir.
            let tid = insert_outbound_with_files(
                &conn,
                terminal_dir.to_str().unwrap(),
                &peer,
                Some("term"),
                &files,
                PackageLayout::Batch,
            )
            .unwrap();
            conn.execute(
                "UPDATE sync_outbound SET state = 'confirmed' WHERE id = ?1",
                [tid],
            )
            .unwrap();
            // (c) non-terminal (announced) row referencing live_dir.
            insert_outbound_with_files(
                &conn,
                live_dir.to_str().unwrap(),
                &peer,
                Some("live"),
                &files,
                PackageLayout::Batch,
            )
            .unwrap();
            // A non-terminal inbound row pins its in-flight tag ("live-wire").
            upsert_inbound_announced(&conn, &peer, "live-wire", 1, 10).unwrap();
        }

        // Seed two in-flight tags on the loopback: one orphan, one with a live row.
        let (ep, runtime) = started_loopback_runtime(&tmp).await;
        ep.seed_in_flight_tag("orphan-wire");
        ep.seed_in_flight_tag("live-wire");

        sweep_transfer_orphans(Arc::new(ctx), Arc::clone(&runtime)).await;

        // Payload dirs: only the row-less orphan is gone.
        assert!(!orphan_dir.exists(), "row-less orphan payload dir removed");
        assert!(
            terminal_dir.exists(),
            "a terminal-row payload dir is kept for Resend"
        );
        assert!(
            live_dir.exists(),
            "a non-terminal-row payload dir is kept for its live attempt"
        );

        // In-flight tags: only the orphan is released; the live-row tag survives.
        assert!(
            !ep.has_in_flight_tag("orphan-wire"),
            "an orphan in-flight tag (no non-terminal inbound row) is released"
        );
        assert!(
            ep.has_in_flight_tag("live-wire"),
            "an in-flight tag whose wire id still has a non-terminal inbound row is kept"
        );
    }

    /// B7 review fix (Critical): the payload-dir sweep age-gates row-less dirs, so
    /// it never races a concurrent enqueue. `write_package` is dir-before-row —
    /// `packages/<uuid>` is created and populated BEFORE the `sync_outbound` row
    /// lands — so a row-less dir alone is not proof of orphanhood; only its AGE
    /// is. A fresh row-less dir (indistinguishable from an in-progress build, or
    /// a row that committed a moment after the sweep's DB snapshot) survives; the
    /// identical dir, once older than [`ORPHAN_SWEEP_MIN_AGE`], is a genuine
    /// prior-session orphan and is removed.
    #[tokio::test]
    async fn sweep_age_gates_rowless_dirs_never_racing_a_concurrent_enqueue() {
        let (_tmp, ctx) = test_ctx();
        let packages_dir = sender_packages_dir(&ctx).unwrap();
        std::fs::create_dir_all(&packages_dir).unwrap();

        let mk = |name: &str| {
            let d = packages_dir.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("frame.fits"), b"payload").unwrap();
            d
        };
        // Left at its natural (just-created) mtime — no row references it, but it
        // is indistinguishable from a live enqueue's dir-before-row window.
        let fresh_dir = mk("fresh-uuid");
        // Backdated past the gate — a genuine orphan from a prior session.
        let old_dir = mk("old-uuid");
        backdate_mtime(&old_dir, ORPHAN_SWEEP_MIN_AGE + Duration::from_secs(60));

        sweep_transfer_orphans(Arc::new(ctx), Arc::new(SyncRuntime::new())).await;

        assert!(
            fresh_dir.exists(),
            "a fresh row-less dir survives the sweep — never raced as a concurrent enqueue"
        );
        assert!(
            !old_dir.exists(),
            "an old row-less dir is a genuine orphan and is removed"
        );
    }

    /// B7 hardening (recursive max mtime): a dir whose TOP-LEVEL entry mtime is
    /// old but which contains one FRESH-mtime file — a live in-flight write, e.g.
    /// a multi-GB master FITS still copying in over slow/NAS-backed storage —
    /// survives the sweep. `dir_age` must read the recursive MAX mtime across the
    /// whole subtree, not just the dir's own entry-creation mtime: the dir's own
    /// mtime only advances on entry creation/removal, so a single file whose copy
    /// outlives [`ORPHAN_SWEEP_MIN_AGE`] would otherwise freeze the dir's own
    /// mtime while write activity is still genuinely live underneath it.
    #[tokio::test]
    async fn sweep_keeps_a_dir_whose_top_level_mtime_is_old_but_has_a_fresh_file() {
        let (_tmp, ctx) = test_ctx();
        let packages_dir = sender_packages_dir(&ctx).unwrap();
        std::fs::create_dir_all(&packages_dir).unwrap();

        let d = packages_dir.join("slow-copy-uuid");
        std::fs::create_dir_all(&d).unwrap();
        // The in-flight file, created first — left at its natural (fresh,
        // just-written) mtime, simulating a large file still being written into.
        std::fs::write(d.join("master.fits"), b"still writing...").unwrap();
        // Explicitly backdate ONLY the dir's own entry past the gate, AFTER the
        // file write above (which would otherwise have bumped the dir fresh too
        // via entry-creation) — pins exactly "old top-level mtime, fresh child",
        // the scenario the pre-hardening top-level-only `dir_age` would have
        // missed (and would have removed this dir despite the live write).
        set_mtime_ago(&d, ORPHAN_SWEEP_MIN_AGE + Duration::from_secs(60));

        sweep_transfer_orphans(Arc::new(ctx), Arc::new(SyncRuntime::new())).await;

        assert!(
            d.exists(),
            "a dir with one fresh-mtime file survives the sweep even though the dir's \
             own entry mtime is old — dir_age reads the recursive MAX mtime, not just \
             the dir's own entry mtime"
        );
    }

    /// D2 §3.5: the button's receive-side half, which never existed. A terminal or
    /// row-less staging tree is dead weight — three receiver failure paths leave
    /// one behind and nothing has ever removed it, so a receiver whose ingest fails
    /// keeps a full second copy of the batch on disk forever. A live or parked
    /// row's staging is untouched: it is the resume target.
    #[tokio::test]
    async fn cleanup_removes_terminal_staging_trees_and_keeps_live_ones() {
        use crate::sync::store::{set_inbound_state, upsert_inbound_announced, CatalogSyncStore};

        let (_tmp, ctx) = test_ctx();
        let sync = SyncRuntime::default();
        let (sync_dir, db_path) = sync_paths(&ctx).unwrap();
        let staging = sync_dir.join("staging");
        let mk = |wire: &str| {
            let d = staging.join(wire);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("payload.fits"), vec![0u8; 1024]).unwrap();
            d
        };
        let done_dir = mk("wire-done");
        let waiting_dir = mk("wire-waiting");
        let orphan_dir = mk("wire-orphan"); // no row at all — a prior session's leak
        {
            let store = CatalogSyncStore::open(&db_path).unwrap();
            let conn = store.lock_conn();
            let peer = "ff".repeat(32);
            upsert_inbound_announced(&conn, &peer, "wire-done", 1, 10).unwrap();
            set_inbound_state(&conn, "wire-done", crate::sync::InboundState::Done, None).unwrap();
            upsert_inbound_announced(&conn, &peer, "wire-waiting", 1, 10).unwrap();
            set_inbound_state(
                &conn,
                "wire-waiting",
                crate::sync::InboundState::Waiting,
                Some("peer gone"),
            )
            .unwrap();
        }

        // The storage figure must see the staging cost BEFORE the sweep, or the
        // button would still be sitting next to a number it cannot move.
        let before = get_transfer_storage(&ctx).unwrap();
        assert!(
            before.staging_bytes >= 3072,
            "staging bytes are part of the footprint: {}",
            before.staging_bytes
        );

        let result = cleanup_finished_transfers(&ctx, &sync).await.unwrap();

        assert!(
            !done_dir.exists(),
            "a terminal row's staging is dead weight"
        );
        assert!(
            !orphan_dir.exists(),
            "so is a staging tree with no row at all"
        );
        assert!(
            waiting_dir.exists(),
            "a parked row's staging is its resume target — unlike its blob tag"
        );
        assert_eq!(result.staging_dirs, 2);
        assert!(
            result.staging_bytes >= 2048,
            "freed now, not on a GC window: {}",
            result.staging_bytes
        );
        assert!(
            get_transfer_storage(&ctx).unwrap().staging_bytes < before.staging_bytes,
            "and the figure the button sits next to actually moves"
        );
    }

    /// D2 §3.5: a `Waiting` row must NOT protect its in-flight blob tag. Today a
    /// dead fetch is terminal, so the sweep releases the tag and the GC reclaims
    /// the partial bytes; if a parked row kept the protection, Clean-up would free
    /// strictly LESS than before D2 — the opposite of what D2 is for. The partial
    /// bytes are not worth an unbounded pin the user cannot clear: the dedup
    /// handshake renegotiates the file list on the next attempt anyway, so at worst
    /// the peer re-sends what the GC took.
    #[tokio::test]
    async fn the_orphan_sweep_releases_a_waiting_rows_tag_but_keeps_a_live_ones() {
        use crate::sync::store::{set_inbound_state, upsert_inbound_announced, CatalogSyncStore};

        let (tmp, ctx) = test_ctx();
        {
            let (_sync_dir, db_path) = sync_paths(&ctx).unwrap();
            let store = CatalogSyncStore::open(&db_path).unwrap();
            let conn = store.lock_conn();
            let peer = "ff".repeat(32);
            // Parked: non-terminal, but NOT live — its tag is reclaimable.
            upsert_inbound_announced(&conn, &peer, "waiting-wire", 1, 10).unwrap();
            set_inbound_state(
                &conn,
                "waiting-wire",
                crate::sync::InboundState::Waiting,
                Some("peer gone"),
            )
            .unwrap();
            // Genuinely fetching: its tag is the live download's protection.
            upsert_inbound_announced(&conn, &peer, "fetching-wire", 1, 10).unwrap();
            set_inbound_state(
                &conn,
                "fetching-wire",
                crate::sync::InboundState::Fetching,
                None,
            )
            .unwrap();
        }

        let (ep, runtime) = started_loopback_runtime(&tmp).await;
        ep.seed_in_flight_tag("waiting-wire");
        ep.seed_in_flight_tag("fetching-wire");

        let released = release_orphan_in_flight_tags(&ctx, &runtime).await.unwrap();

        assert_eq!(released, 1, "exactly the parked row's tag");
        assert!(
            !ep.has_in_flight_tag("waiting-wire"),
            "a parked row's partial download is reclaimable — it is not a live fetch"
        );
        assert!(
            ep.has_in_flight_tag("fetching-wire"),
            "a live fetch still owns its tag"
        );
    }

    /// B7 cleanup: `cleanup_finished_transfers` removes TERMINAL rows' payloads
    /// (their `resendable` honestly flips false), keeps non-terminal payloads, and
    /// the storage stats drop across the call.
    #[tokio::test]
    async fn cleanup_removes_terminal_payloads_and_flips_resendable() {
        use crate::sharing::types::AnnounceFileEntry;
        use crate::sync::store::insert_outbound_with_files;

        let (_tmp, ctx) = test_ctx();
        let packages_dir = sender_packages_dir(&ctx).unwrap();
        std::fs::create_dir_all(&packages_dir).unwrap();
        // `resendable` requires a real payload dir (manifest + a payload file), the
        // exact shape `package_has_payload` accepts.
        let mk = |name: &str| {
            let d = packages_dir.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(package::MANIFEST_FILENAME), b"{}\n").unwrap();
            std::fs::write(d.join("frame.fits"), b"payload-bytes").unwrap();
            d
        };
        let failed_dir = mk("failed-uuid"); // terminal → reclaimed
        let live_dir = mk("live-uuid"); // non-terminal → kept

        let peer = "bb".repeat(32);
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let files = vec![AnnounceFileEntry {
                rel_path: "L/frame.fits".into(),
                byte_size: 13,
                frame_uuid: "f".into(),
            }];
            // A FAILED terminal row with its payload still on disk → resendable now.
            let fid = insert_outbound_with_files(
                &conn,
                failed_dir.to_str().unwrap(),
                &peer,
                Some("failed"),
                &files,
                PackageLayout::Batch,
            )
            .unwrap();
            conn.execute(
                "UPDATE sync_outbound SET state = 'failed' WHERE id = ?1",
                [fid],
            )
            .unwrap();
            insert_outbound_with_files(
                &conn,
                live_dir.to_str().unwrap(),
                &peer,
                Some("live"),
                &files,
                PackageLayout::Batch,
            )
            .unwrap();
        }

        // Before: the failed row is resendable (payload present); stats see 2 dirs.
        let before = list_terminal_transfers(&ctx, None).expect("terminal transfers");
        let failed_summary = before
            .sent
            .iter()
            .find(|s| s.batch_uuid == "failed-uuid")
            .expect("failed summary present");
        assert!(
            failed_summary.resendable,
            "a failed row with payload is resendable before cleanup"
        );
        let stats_before = get_transfer_storage(&ctx).expect("stats before");
        assert_eq!(
            stats_before.packages_count, 2,
            "two package dirs before cleanup"
        );

        let result = cleanup_finished_transfers(&ctx, &SyncRuntime::new())
            .await
            .expect("cleanup ok");
        assert_eq!(result.payload_dirs, 1, "one terminal payload dir reclaimed");
        assert!(result.payload_bytes > 0, "reclaimed some bytes");
        assert_eq!(
            result.tags_released, 0,
            "no transport started → no tags released"
        );

        // After: terminal payload gone, live payload kept, resendable flipped false.
        assert!(
            !failed_dir.exists(),
            "cleanup removed the terminal payload dir"
        );
        assert!(
            live_dir.exists(),
            "cleanup kept the non-terminal payload dir"
        );
        let after = list_terminal_transfers(&ctx, None).expect("terminal transfers after");
        let failed_after = after
            .sent
            .iter()
            .find(|s| s.batch_uuid == "failed-uuid")
            .expect("failed summary still present");
        assert!(
            !failed_after.resendable,
            "resendable honestly flips false once the payload is reclaimed"
        );
        let stats_after = get_transfer_storage(&ctx).expect("stats after");
        assert_eq!(
            stats_after.packages_count, 1,
            "one package dir remains after cleanup"
        );
        assert!(
            stats_after.packages_bytes < stats_before.packages_bytes,
            "packages_bytes drops after cleanup"
        );
    }

    /// Batch Model §D5: the status summaries carry the new `generation` (attempt N)
    /// and `batch_uuid` fields — sent (basename == batch_uuid) and received (the
    /// row's column, falling back to the wire id when NULL).
    #[tokio::test]
    async fn summaries_carry_generation_and_batch_uuid() {
        use crate::sharing::types::AnnounceFileEntry;
        use crate::sync::store::{insert_outbound_with_files, upsert_inbound_announced};
        use crate::sync::InboundState;

        let (_tmp, ctx) = test_ctx();
        let peer = "aa".repeat(32);
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            // A terminal sent row whose package_ref basename is the batch key.
            let files = vec![AnnounceFileEntry {
                rel_path: "L/a.fits".into(),
                byte_size: 4,
                frame_uuid: "a".into(),
            }];
            let sid = insert_outbound_with_files(
                &conn,
                "/pkgs/batch-out",
                &peer,
                Some("Out"),
                &files,
                PackageLayout::Batch,
            )
            .unwrap();
            conn.execute(
                "UPDATE sync_outbound SET state = 'confirmed' WHERE id = ?1",
                [sid],
            )
            .unwrap();
            // A terminal received row with a NULL batch_uuid (upsert_inbound_announced
            // leaves it NULL) → batch_uuid falls back to the wire id.
            let rid = upsert_inbound_announced(&conn, &peer, "wire-in", 1, 10).unwrap();
            crate::sync::store::set_inbound_state(&conn, "wire-in", InboundState::Done, None)
                .unwrap();
            let _ = rid;
        }

        let terminal = list_terminal_transfers(&ctx, None).expect("terminal transfers");
        let sent = terminal
            .sent
            .iter()
            .find(|s| s.batch_uuid == "batch-out")
            .expect("sent summary present");
        assert_eq!(sent.generation, 1, "a fresh sent row is at generation 1");
        assert_eq!(
            sent.batch_uuid, "batch-out",
            "sent batch_uuid == package-dir basename"
        );

        let recv = terminal
            .received
            .iter()
            .find(|r| r.package_id == "wire-in")
            .expect("received summary present");
        assert_eq!(
            recv.generation, 1,
            "a fresh received row is at generation 1"
        );
        assert_eq!(
            recv.batch_uuid, "wire-in",
            "a NULL-batch_uuid received row falls back to the wire id"
        );
    }

    /// Task 3.3: with NO node bound and NOT signed in, transport health reads
    /// `not_started` — the poll must not bind a node, and the ctx's node slot
    /// stays `None` afterward (peek-only).
    #[tokio::test]
    async fn transport_health_no_node_is_not_started() {
        let (_tmp, ctx) = test_ctx();
        let health = derive_transport_health(&ctx).await.expect("derive health");
        assert_eq!(health.status, "not_started");
        assert!(
            ctx.iroh_node.lock().await.is_none(),
            "deriving health must never lazily bind a node"
        );
    }

    /// Task 3.3: signed in but with an empty cached relay map and no node bound,
    /// transport health reads `no_relay_map` — the stuck state worth surfacing
    /// (transfers to remote peers would stall). Local settings only; no hub call.
    #[tokio::test]
    async fn transport_health_signed_in_empty_relays_is_no_relay_map() {
        let (_tmp, ctx) = test_ctx();
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            crate::db::set_setting(&conn, keys::ACCOUNT_DEVICE_ID, "device-1").unwrap();
        }
        let health = derive_transport_health(&ctx).await.expect("derive health");
        assert_eq!(health.status, "no_relay_map");
    }

    /// Task 3 (Д2): `ensure_iroh_node` lazily binds the ONE shared node and
    /// caches it on the ctx — two ensure calls return the SAME `Arc`, and a
    /// host-style shutdown (take + `shutdown`) lets the next ensure re-bind a
    /// FRESH node over the same sync dir (the device-key lock + store were
    /// released cleanly). A bogus cached relay forces `resolve_relay_mode` →
    /// `RelayMode::Custom` without a hub round-trip and without iroh's public
    /// default relays; the endpoint binds locally (no role is ever started, so no
    /// relay connection is attempted) and `.invalid` is guaranteed non-resolvable
    /// (RFC 2606), keeping the test hermetic.
    #[tokio::test]
    async fn ensure_iroh_node_caches_then_rebinds_after_shutdown() {
        let (_tmp, ctx) = test_ctx();
        store_cached_relays(&ctx, &["https://relay.invalid".to_string()]);

        let node1 = ensure_iroh_node(&ctx).await.expect("first bind");
        let node2 = ensure_iroh_node(&ctx).await.expect("second ensure reuses");
        assert!(
            Arc::ptr_eq(&node1, &node2),
            "two ensure calls must return the same cached node Arc"
        );

        // Host-style teardown: take the node out of the ctx and shut it down
        // (releases the device-key lock + closes the store), clearing the slot.
        let taken = ctx
            .iroh_node
            .lock()
            .await
            .take()
            .expect("node present on the ctx");
        assert!(Arc::ptr_eq(&taken, &node1), "the ctx held the same node");
        taken.shutdown().await;

        // Next ensure re-binds a fresh node (different Arc) over the same dir.
        let node3 = ensure_iroh_node(&ctx)
            .await
            .expect("re-bind after shutdown");
        assert!(
            !Arc::ptr_eq(&node1, &node3),
            "an ensure after shutdown must bind a fresh node, not the shut-down one"
        );

        let taken3 = ctx.iroh_node.lock().await.take().expect("node3 present");
        taken3.shutdown().await;
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
            StartedSender {
                engine,
                origin_device: node_id_hex(&peer),
                peer,
            }
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
        assert!(
            sender.current_for(&c).await.is_none(),
            "unknown peer has no engine"
        );
        assert_eq!(sender.started_peers().await.len(), 2);
    }

    /// tv2 follow-up (sender resurrection): the startup enumeration collects the
    /// DISTINCT peers of the non-terminal outbound rows. Terminal rows are excluded
    /// by `non_terminal()` (so a peer whose every row is terminal never appears),
    /// and a peer with several non-terminal rows appears exactly once, in first-seen
    /// (id-ascending) order.
    #[test]
    fn distinct_pending_peers_dedups_and_excludes_terminal() {
        use crate::sync::StandaloneSyncStore;

        let tmp = tempfile::tempdir().unwrap();
        let store = StandaloneSyncStore::open(tmp.path().join("peers.db")).unwrap();
        let node = |b: u8| crate::sync::node_id_from_hex(&format!("{b:02x}").repeat(32)).unwrap();
        let peer_a = node(0xaa);
        let peer_b = node(0xbb);
        let peer_c = node(0xcc);

        // peer_a: two non-terminal rows (queued + transferring).
        store
            .enqueue("/pkgs/a1", peer_a, None, &[], PackageLayout::Batch)
            .unwrap();
        let a2 = store
            .enqueue("/pkgs/a2", peer_a, None, &[], PackageLayout::Batch)
            .unwrap();
        store.set_state(a2, OutboundState::Transferring).unwrap();
        // peer_b: one non-terminal row + one terminal (confirmed).
        store
            .enqueue("/pkgs/b1", peer_b, None, &[], PackageLayout::Batch)
            .unwrap();
        let b2 = store
            .enqueue("/pkgs/b2", peer_b, None, &[], PackageLayout::Batch)
            .unwrap();
        store.set_state(b2, OutboundState::Confirmed).unwrap();
        // peer_c: every row terminal (failed) → must be excluded entirely.
        let c1 = store
            .enqueue("/pkgs/c1", peer_c, None, &[], PackageLayout::Batch)
            .unwrap();
        store.set_state(c1, OutboundState::Failed).unwrap();

        let rows = store.non_terminal().unwrap();
        let peers = distinct_pending_peers(&rows);
        assert_eq!(
            peers,
            vec![peer_a, peer_b],
            "distinct non-terminal peers, first-seen order; the all-terminal peer_c is excluded"
        );
    }

    /// tv2 follow-up review fix: `sync_outbound` has no personal/project
    /// discriminator, so a peer whose only pending rows are collab/project rows
    /// (including a cross-account collaborator) can end up in the enumerated peer
    /// set alongside real account members. `filter_account_peers` must keep only
    /// the peer present in the cached account allow-list and drop the rest — pure,
    /// no DB/iroh involved, so the filtering logic itself is directly testable.
    #[test]
    fn filter_account_peers_keeps_only_authorized() {
        let node = |b: u8| crate::sync::node_id_from_hex(&format!("{b:02x}").repeat(32)).unwrap();
        let member = node(0xaa);
        let stranger = node(0xbb);
        let authorized: HashSet<String> = [node_id_hex(&member)].into_iter().collect();

        let kept = filter_account_peers(vec![member, stranger], &authorized);
        assert_eq!(
            kept,
            vec![member],
            "only the account member survives the filter"
        );
    }

    /// tv2 follow-up review fix, end-to-end at the `_with` seam: two peers have
    /// pending rows — `member` (in the cached `SYNC_AUTHORIZED_PEERS` allow-list)
    /// and `stranger` (not present — stand-in for a cross-account collaborator's
    /// node, since `sync_outbound` cannot itself distinguish a personal row from a
    /// collab row). With `authorized = Some(..)` only `member` is resurrected —
    /// the builder is invoked exactly once, for `member`, and never for
    /// `stranger`. No iroh/loopback transport needed: `build` is a plain counting
    /// closure, proving the enumeration+filter wiring in isolation from the engine
    /// build itself.
    #[tokio::test]
    async fn resurrect_pending_senders_with_skips_non_account_peers() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let node = |b: u8| crate::sync::node_id_from_hex(&format!("{b:02x}").repeat(32)).unwrap();
        let member = node(0xaa);
        let stranger = node(0xbb);

        {
            let store = CatalogSyncStore::open(&db_path).unwrap();
            store
                .enqueue("/pkgs/member", member, None, &[], PackageLayout::Batch)
                .unwrap();
            store
                .enqueue("/pkgs/stranger", stranger, None, &[], PackageLayout::Batch)
                .unwrap();
        }

        let authorized: HashSet<String> = [node_id_hex(&member)].into_iter().collect();
        let sender = Arc::new(SyncSenderRuntime::new());
        let called: Arc<std::sync::Mutex<Vec<NodeId>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let called_in = Arc::clone(&called);
        let ensured =
            resurrect_pending_senders_with(&db_path, &sender, Some(&authorized), move |peer| {
                let called = Arc::clone(&called_in);
                async move {
                    called.lock().unwrap().push(peer);
                    Ok::<(), ApiError>(())
                }
            })
            .await
            .expect("resurrect ok");

        assert_eq!(ensured, 1, "only the account member is resurrected");
        assert_eq!(
            *called.lock().unwrap(),
            vec![member],
            "the builder is invoked only for the authorized peer, never the stranger"
        );
    }

    /// Multi-destination regression (sync 2C / Phase 3): with two started
    /// per-peer engines, `build_sender_status` must NOT N-duplicate the in-flight
    /// rows. Each engine opens its OWN `CatalogSyncStore` over the SAME catalog
    /// DB — exactly how `ensure_sender_engine` builds them — and
    /// `non_terminal()` has no peer filter, so every engine's snapshot returns
    /// the full non-terminal set. Rolled up naively across 2 engines that would
    /// be 4 rows for 2 real packages; the fix dedups by row id.
    ///
    /// The two non-terminal rows are addressed to a THIRD peer X, so neither
    /// started engine (A, B) re-drives them on crash-resume (`row.peer !=
    /// self.peer`) — keeping the rows stable through the read while still
    /// reproducing the cross-peer duplication.
    #[tokio::test]
    async fn sender_status_dedupes_active_rows_across_peers() {
        use crate::sharing::loopback::LoopbackNetwork;
        use crate::sharing::SharingTransport;

        let (_tmp, ctx) = test_ctx();
        let db_path = db(&ctx).unwrap().path().to_path_buf();
        let node = |b: u8| crate::sync::node_id_from_hex(&format!("{b:02x}").repeat(32)).unwrap();

        // Two non-terminal rows (ids 1, 2) in the shared catalog sync store,
        // addressed to a third peer X: one Queued, one Transferring.
        let peer_x = node(0xcc);
        {
            let store = CatalogSyncStore::open(&db_path).unwrap();
            let id1 = store
                .enqueue("/pkgs/one", peer_x, None, &[], PackageLayout::Batch)
                .unwrap();
            let id2 = store
                .enqueue("/pkgs/two", peer_x, None, &[], PackageLayout::Batch)
                .unwrap();
            assert_eq!((id1, id2), (1, 2));
            store.set_state(id2, OutboundState::Transferring).unwrap();
        }

        // Two started per-peer engines (A, B), each with its OWN store over the
        // SAME catalog DB, idling with nothing of their own enqueued.
        let started_for = |peer: NodeId| {
            let net = LoopbackNetwork::new();
            let store = Arc::new(CatalogSyncStore::open(&db_path).unwrap());
            let engine = Arc::new(SyncEngine::spawn(
                store as Arc<dyn SyncStore>,
                Arc::new(net.endpoint()) as Arc<dyn SharingTransport>,
                peer,
            ));
            StartedSender {
                engine,
                origin_device: node_id_hex(&peer),
                peer,
            }
        };
        let peer_a = node(0xaa);
        let peer_b = node(0xbb);
        let sender = SyncSenderRuntime::new();
        {
            let mut g = sender.lock_inner().await;
            g.insert(peer_a, started_for(peer_a));
            g.insert(peer_b, started_for(peer_b));
        }

        let status = build_sender_status(&ctx, &sender).await.unwrap();
        assert_eq!(
            status.active.len(),
            2,
            "2 distinct rows, not 4 (deduped across the 2 peers)"
        );
        assert_eq!(
            status.active.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![1, 2],
            "deduped rows stay ordered ascending by id"
        );
        assert_eq!(status.queued, 1, "one Queued row, counted once");
        assert_eq!(status.transferring, 1, "one Transferring row, counted once");
        assert_eq!(status.queued + status.transferring, 2);
    }

    /// Variant A honest-labeling guard, api half: the package the receiver is
    /// ACTUALLY pulling is never the one described as queued behind something. Only
    /// its SIBLINGS to that same peer are.
    #[test]
    fn only_the_siblings_of_the_served_package_are_queued_at_the_receiver() {
        // The package being served right now.
        assert!(
            !row_receiver_busy(7, Some(7)),
            "the moving package must never read as queued behind itself"
        );
        // A sibling to the same peer, waiting its turn in that peer's serial lane.
        assert!(row_receiver_busy(8, Some(7)));
        // A peer with no live serve signal (idle, or the signal decayed) leaves every
        // row on today's behavior.
        assert!(!row_receiver_busy(8, None));
    }

    /// The receiver's allow-list is now every device in the account (mesh model,
    /// finding H1 as updated for sync Phase 1) — regardless of a device's
    /// capability (full Athenaeum peer or send-only Perseus agent).
    #[test]
    fn account_peer_hexes_includes_every_device_regardless_of_role() {
        use crate::account::{AccountDevice, DeviceCapability};
        use base64::Engine;
        let b64 = |bytes: [u8; 32]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let dev = |seed: u8, capability: DeviceCapability| AccountDevice {
            id: format!("dev-{seed}"),
            name: format!("n{seed}"),
            pubkey: b64([seed; 32]),
            capability,
            created_at: "2026-07-11T00:00:00Z".into(),
            last_seen_at: None,
            endpoint_addr: None,
        };
        let devices = vec![
            dev(1, DeviceCapability::Athenaeum),
            dev(2, DeviceCapability::Perseus),
            dev(3, DeviceCapability::Athenaeum),
        ];
        let hexes = account_peer_hexes(&devices);
        assert_eq!(
            hexes.len(),
            3,
            "every account device is authorized, not just paired captures"
        );
        assert!(hexes.contains(&"01".repeat(32)));
        assert!(hexes.contains(&"02".repeat(32)));
        assert!(hexes.contains(&"03".repeat(32)));

        // The device-names cache maps each decodable pubkey's hex → its current
        // name — the source the receiver reads to name incoming landing folders.
        let names = account_device_names(&devices);
        assert_eq!(names.len(), 3);
        assert_eq!(names.get(&"01".repeat(32)).map(String::as_str), Some("n1"));
        assert_eq!(names.get(&"02".repeat(32)).map(String::as_str), Some("n2"));
        assert_eq!(names.get(&"03".repeat(32)).map(String::as_str), Some("n3"));
    }

    /// Perseus UI v2 (Task 9): the capability cache maps each decodable pubkey's
    /// hex → its device capability string — the source the receiver reads to stamp
    /// an inbound row so the Transfers UI can tell an Athenaeum peer from a Perseus
    /// agent. Undecodable pubkeys are skipped, same as the names/hex maps.
    #[test]
    fn account_device_capabilities_maps_hex_to_capability_string() {
        use crate::account::{AccountDevice, DeviceCapability};
        use base64::Engine;
        let b64 = |bytes: [u8; 32]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let dev = |seed: u8, capability: DeviceCapability| AccountDevice {
            id: format!("dev-{seed}"),
            name: format!("n{seed}"),
            pubkey: b64([seed; 32]),
            capability,
            created_at: "2026-07-22T00:00:00Z".into(),
            last_seen_at: None,
            endpoint_addr: None,
        };
        let devices = vec![
            dev(1, DeviceCapability::Athenaeum),
            dev(2, DeviceCapability::Perseus),
        ];
        let caps = account_device_capabilities(&devices);
        assert_eq!(caps.len(), 2);
        assert_eq!(
            caps.get(&"01".repeat(32)).map(String::as_str),
            Some("athenaeum")
        );
        assert_eq!(
            caps.get(&"02".repeat(32)).map(String::as_str),
            Some("perseus")
        );

        // An undecodable pubkey is skipped, never a partial/garbage entry.
        let mut bad = dev(3, DeviceCapability::Perseus);
        bad.pubkey = "not-base64!!".into();
        let caps = account_device_capabilities(&[bad]);
        assert!(
            caps.is_empty(),
            "an undecodable pubkey yields no capability entry"
        );
    }

    /// The relay-map cache round-trips through settings the same way.
    #[test]
    fn cached_relays_round_trip() {
        let (_tmp, ctx) = test_ctx();
        assert_eq!(cached_relays(&ctx).unwrap(), Vec::<String>::new());

        let urls = vec![
            "https://relay1.example.org".to_string(),
            "https://relay2.example.org".to_string(),
        ];
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
            crate::db::set_setting(&conn, keys::ACCOUNT_HUB_URL, "http://sync-test.invalid")
                .unwrap();
        }
        let names = get_sync_device_names(&ctx).await.unwrap();
        assert!(
            names.is_empty(),
            "a signed-out device resolves to an empty map, not an error"
        );
    }

    /// tv2 §D5/§D8: the status path resolves device names from the cached
    /// `SYNC_DEVICE_NAMES` settings map (present / absent / malformed) with NO hub
    /// round-trip — a settings-only, best-effort read.
    #[test]
    fn device_names_cache_resolves_present_absent_and_malformed() {
        let (_tmp, ctx) = test_ctx();
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let peer_a = "aa".repeat(32);
        let peer_z = "99".repeat(32);

        // Absent setting → empty map (falls back to hex at the call site).
        assert!(
            cached_device_names(&conn).is_empty(),
            "no cache yet → empty"
        );

        // Present → hex→name lookups; an uncached hex is absent.
        let json = format!(r#"{{"{peer_a}":"My Mac"}}"#);
        crate::db::set_setting(&conn, keys::SYNC_DEVICE_NAMES, &json).unwrap();
        let map = cached_device_names(&conn);
        assert_eq!(map.get(&peer_a).map(String::as_str), Some("My Mac"));
        assert!(
            map.get(&peer_z).is_none(),
            "an uncached peer resolves to None"
        );

        // Malformed JSON → empty map, never a panic/error.
        crate::db::set_setting(&conn, keys::SYNC_DEVICE_NAMES, "not json").unwrap();
        assert!(
            cached_device_names(&conn).is_empty(),
            "malformed cache → empty, best-effort"
        );
    }

    /// tv2 §D4: `list_transfer_files` reads the persisted per-file tables as its
    /// primary source — a v2 inbound batch returns its `announced` rows (rel_path
    /// + state + bytes_done) before any fetch — and falls back to the legacy
    /// history source for a pre-v2 batch that has no per-file rows.
    #[test]
    fn list_transfer_files_v2_rows_then_legacy_fallback() {
        use crate::sync::{InboundFileRow, InboundFileState, InboundState};
        let (_tmp, ctx) = test_ctx();
        let peer = "aa".repeat(32);

        // A v2 inbound batch: announced rows written at announce time, no fetch yet.
        let v2_id = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let id = crate::sync::store::upsert_inbound_announced(&conn, &peer, "pkg-v2", 2, 300)
                .unwrap();
            let mk = |rel: &str, size: u64| InboundFileRow {
                inbound_id: id,
                rel_path: rel.into(),
                byte_size: size,
                frame_uuid: format!("u-{rel}"),
                state: InboundFileState::Announced,
                bytes_done: 0,
                outcome: None,
                error: None,
                updated_at: "2026-07-20T00:00:00.000Z".into(),
            };
            crate::sync::store::replace_inbound_files(
                &conn,
                id,
                &[mk("cam/lights/b.fits", 200), mk("cam/lights/a.fits", 100)],
            )
            .unwrap();
            id
        };
        let files = list_transfer_files(&ctx, Direction::Received, v2_id).unwrap();
        assert_eq!(
            files.len(),
            2,
            "both announced rows surface before any fetch"
        );
        // Ordered by rel_path; forward-slash rel_path preserved, basename in `name`.
        assert_eq!(files[0].rel_path, "cam/lights/a.fits");
        assert_eq!(files[0].name, "a.fits");
        assert_eq!(files[0].state.as_deref(), Some("announced"));
        assert_eq!(files[0].bytes_done, Some(0));
        assert!(files[0].outcome.is_none());
        assert!(files[0].error.is_none());

        // A legacy inbound batch: NO per-file rows, terminal, with received history.
        let legacy_id = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let id =
                crate::sync::store::upsert_inbound_announced(&conn, &peer, "pkg-legacy", 1, 50)
                    .unwrap();
            crate::sync::store::set_inbound_state(&conn, "pkg-legacy", InboundState::Done, None)
                .unwrap();
            crate::sync::store::insert_history_row(
                &conn,
                &HistoryRow {
                    frame_uuid: "legacy-uuid".into(),
                    filename: "old.fits".into(),
                    object: Some("M31".into()),
                    peer_device: peer.clone(),
                    direction: Direction::Received,
                    bytes: 50,
                    started_at: "2026-01-01T00:00:00.000Z".into(),
                    finished_at: Some("2026-01-01T00:00:01.000Z".into()),
                    outcome: "ingested".into(),
                    project: None,
                    package_id: Some("pkg-legacy".into()),
                    batch_name: None,
                },
            )
            .unwrap();
            id
        };
        let legacy = list_transfer_files(&ctx, Direction::Received, legacy_id).unwrap();
        assert_eq!(legacy.len(), 1, "falls back to received history");
        assert_eq!(legacy[0].name, "old.fits");
        assert_eq!(
            legacy[0].rel_path, "old.fits",
            "no rel_path in history → basename"
        );
        assert_eq!(legacy[0].outcome.as_deref(), Some("ingested"));
        assert!(
            legacy[0].state.is_none(),
            "legacy fallback has no per-file state"
        );
    }

    /// tv2 §D7: `list_transfer_events` returns a batch's journal newest-first, in
    /// the `{ ts, kind, detail }` shape, scoped to the (direction, row id) key.
    #[test]
    fn list_transfer_events_returns_newest_first() {
        let (_tmp, ctx) = test_ctx();
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            for (kind, detail) in [
                ("enqueued", Some("frames=2")),
                ("announce_sent", None),
                ("uploaded", Some("bytes=300")),
            ] {
                crate::sync::store::append_sync_event(&conn, Direction::Sent, "7", kind, detail)
                    .unwrap();
            }
            // A different batch's journal must not leak into batch 7.
            crate::sync::store::append_sync_event(&conn, Direction::Sent, "8", "enqueued", None)
                .unwrap();
        }
        let events = list_transfer_events(&ctx, Direction::Sent, 7).unwrap();
        assert_eq!(events.len(), 3, "only batch 7's events");
        assert_eq!(events[0].kind, "uploaded", "newest first");
        assert_eq!(events[0].detail.as_deref(), Some("bytes=300"));
        assert_eq!(events[2].kind, "enqueued", "oldest last");
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
            content_hash: None,
            archived_in_operation: None,
            archive_zip_path: None,
            archive_path_in_zip: None,
            uuid: None,
            updated_at: None,
        };
        let file_id = crate::db::insert_file(&conn, &file).unwrap();
        let frame = Frame {
            file_id,
            object: Some(object.to_string()),
            ..Default::default()
        };
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
            build_selection_package(
                &conn,
                "origin-dev",
                &pkg_root,
                selection_entries(&conn, &[f1, f2], None).unwrap(),
                None,
                None,
            )
            .unwrap()
        };
        assert_eq!(built.eligible.len(), 2);
        assert!(built.ineligible.is_empty());
        assert_eq!(built.total, 2);

        let dir = built.pkg_dir.expect("a package was written");
        let records = crate::package::read_manifest(&dir).unwrap();
        assert_eq!(
            records.len(),
            2,
            "exactly the selection, not the unselected f3"
        );
        for r in &records {
            let f: crate::models::Frame = serde_json::from_value(r.frame_meta.clone()).unwrap();
            assert_eq!(
                f.object.as_deref(),
                Some("M42"),
                "frame_meta is the catalog Frame"
            );
        }
        let analyzed: Vec<&crate::package::ManifestRecord> =
            records.iter().filter(|r| r.analysis.is_some()).collect();
        assert_eq!(
            analyzed.len(),
            1,
            "analysis summary included only when present"
        );
        let af: crate::models::Frame =
            serde_json::from_value(analyzed[0].frame_meta.clone()).unwrap();
        assert_eq!(
            af.id,
            Some(f1),
            "the analysis is attached to the analyzed frame"
        );
    }

    /// Task M4: building a selection package records the retention linkage in
    /// `sync_sources` — one live row per eligible frame, keyed on the SAME
    /// Building a package hashes every source file in full for the manifest.
    /// That read is banked into `files.strong_hash` for every row the disk
    /// still vouches for (`(size, modified_at)` match — the contract shared
    /// with the master-hash pass and deep verify); a row that lies about its
    /// bytes is left alone. The fixture stores `Utc::now()` as `modified_at`,
    /// which never matches the file's mtime, so the current row here is
    /// re-stamped with the real stat first.
    #[test]
    fn build_selection_banks_strong_hash_into_current_rows() {
        let (tmp, ctx) = test_ctx();
        let dir = tmp.path();
        let current = insert_fixture_frame(&ctx, dir, "current.fits", "M42", false);
        let stale = insert_fixture_frame(&ctx, dir, "stale.fits", "M42", false);
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let (current_file_id, current_path): (i64, String) = conn
            .query_row(
                "SELECT f.id, f.path FROM files f JOIN frames fr ON fr.file_id = f.id WHERE fr.id = ?1",
                [current],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let meta = std::fs::metadata(&current_path).unwrap();
        let mtime = chrono::DateTime::<chrono::Utc>::from(meta.modified().unwrap()).to_rfc3339();
        conn.execute(
            "UPDATE files SET size = ?2, modified_at = ?3 WHERE id = ?1",
            rusqlite::params![current_file_id, meta.len() as i64, mtime],
        )
        .unwrap();
        // The stale row keeps the fixture's fictional mtime AND lies about its size.
        let stale_file_id: i64 = conn
            .query_row("SELECT file_id FROM frames WHERE id = ?1", [stale], |r| r.get(0))
            .unwrap();
        conn.execute("UPDATE files SET size = size + 7 WHERE id = ?1", [stale_file_id])
            .unwrap();

        let pkg_root = dir.join("packages");
        let built = build_selection_package(
            &conn,
            "origin-dev",
            &pkg_root,
            selection_entries(&conn, &[current, stale], None).unwrap(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(built.eligible.len(), 2, "both files exist on disk, both are packaged");
        let expect = crate::package::xxh3_full_file(std::path::Path::new(&current_path)).unwrap();

        let banked: Option<String> = conn
            .query_row("SELECT strong_hash FROM files WHERE id = ?1", [current_file_id], |r| r.get(0))
            .unwrap();
        assert_eq!(banked.as_deref(), Some(expect.as_str()), "current row banks the manifest hash");
        let untouched: Option<String> = conn
            .query_row("SELECT strong_hash FROM files WHERE id = ?1", [stale_file_id], |r| r.get(0))
            .unwrap();
        assert_eq!(untouched, None, "a row the disk does not vouch for is never written");
    }

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
        let built = build_selection_package(
            &conn,
            "origin-dev",
            &pkg_root,
            selection_entries(&conn, &[f1, f2], None).unwrap(),
            None,
            None,
        )
        .unwrap();
        let pkg_ref = built
            .pkg_dir
            .clone()
            .expect("a package was written")
            .to_string_lossy()
            .to_string();

        let sources = crate::sync::live_sources_for_package(&conn, &pkg_ref).unwrap();
        assert_eq!(
            sources.len(),
            1,
            "one linkage row for the single eligible frame (f2 was missing)"
        );
        let row = &sources[0];
        assert_eq!(
            row.path,
            dir.join("light-0001.fits").to_string_lossy(),
            "linkage points at the source path"
        );
        assert!(
            row.file_id.is_some(),
            "the catalog file_id is recorded for a catalog-consistent delete"
        );
        assert_eq!(
            row.size,
            std::fs::metadata(dir.join("light-0001.fits"))
                .unwrap()
                .len(),
            "recorded size matches disk"
        );
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
            build_selection_package(
                &conn,
                "origin-dev",
                &pkg_root,
                selection_entries(&conn, &[f1, f2, unknown], None).unwrap(),
                None,
                None,
            )
            .unwrap()
        };
        assert_eq!(
            built.eligible,
            vec![f1],
            "only the present file is eligible"
        );
        assert_eq!(built.total, 3);
        let reasons: HashMap<i64, String> = built
            .ineligible
            .iter()
            .map(|i| (i.frame_id, i.reason.clone()))
            .collect();
        assert!(
            reasons.get(&f2).unwrap().contains("missing"),
            "f2 reported missing"
        );
        assert!(
            reasons.get(&unknown).unwrap().contains("not found"),
            "unknown id reported"
        );

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
        // `enqueue_sync_selection` now takes an owned `Arc<ServiceContext>` (it
        // builds a `'static` retry address-refresher from it, T8).
        let ctx = Arc::new(ctx);
        let sender = Arc::new(SyncSenderRuntime::new());
        let collab_sender = Arc::new(SyncSenderRuntime::new());
        let sync = SyncRuntime::new();
        let dest = ResolvedDest {
            node: [7u8; 32],
            endpoint_addr: None,
        };

        let result = enqueue_sync_selection(
            &ctx,
            &sender,
            collab_sender,
            &sync,
            dest,
            Vec::new(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.enqueued_count, 0);
        assert_eq!(result.eligible_count, 0);
        assert_eq!(result.total_count, 0);
        assert!(result.ineligible.is_empty());
        assert!(
            !sender.is_started().await,
            "no engine started for an empty selection"
        );
        assert!(
            sender.started_peers().await.is_empty(),
            "no peer engine for an empty selection"
        );
    }

    // ── Retry / send-now / cancel command surface (Task 8) ───────────────────

    /// Build a real one-frame package (manifest + payload copied into the dir) in
    /// the ctx's catalog and return its dir — the retry precondition needs a dir
    /// `package_has_payload` accepts.
    fn build_one_frame_package(ctx: &ServiceContext, tmp: &Path) -> PathBuf {
        let f1 = insert_fixture_frame(ctx, tmp, "retry-0001.fits", "M42", false);
        let pkg_root = tmp.join("packages");
        let db = db(ctx).unwrap();
        let conn = db.conn();
        build_selection_package(
            &conn,
            "origin-dev",
            &pkg_root,
            selection_entries(&conn, &[f1], None).unwrap(),
            None,
            None,
        )
        .unwrap()
        .pkg_dir
        .expect("a package was written")
    }

    /// A loopback sender engine bound to the SAME catalog store the api fns read,
    /// keyed under a made-up peer in a fresh runtime. The peer never starts an
    /// endpoint, so the engine's announce fails fast ("peer not started") and the
    /// row parks in `Queued` (non-terminal) — enough to cancel, with no receiver
    /// endpoint to keep alive. The long ack timeout keeps a timeout from racing a
    /// cancel.
    async fn loopback_sender_for(db_path: &Path) -> (Arc<SyncSenderRuntime>, NodeId) {
        use crate::sharing::loopback::LoopbackNetwork;
        use crate::sharing::SharingTransport;
        use crate::sync::SyncConfig;

        let net = LoopbackNetwork::new();
        let peer: NodeId = crate::sync::node_id_from_hex(&"ab".repeat(32)).unwrap();
        let store = Arc::new(CatalogSyncStore::open(db_path).unwrap());
        let engine = Arc::new(SyncEngine::spawn_with_config(
            store as Arc<dyn SyncStore>,
            Arc::new(net.endpoint()) as Arc<dyn SharingTransport>,
            peer,
            SyncConfig {
                ack_timeout: Duration::from_secs(60),
            },
        ));
        let sender = Arc::new(SyncSenderRuntime::new());
        sender.lock_inner().await.insert(
            peer,
            StartedSender {
                engine,
                origin_device: node_id_hex(&peer),
                peer,
            },
        );
        (sender, peer)
    }

    /// Poll the shared catalog store until outbound row `id` reaches `want`.
    async fn wait_outbound_state(db_path: &Path, id: i64, want: OutboundState) {
        for _ in 0..500 {
            let store = CatalogSyncStore::open(db_path).unwrap();
            if store.get_outbound(id).unwrap().map(|r| r.state) == Some(want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for outbound {id} to reach {want:?}");
    }

    /// Transfers Batch Model §D1/§D3: `resend_transfer` (aliased by
    /// `retry_sync_package`) resets the SAME terminal row in place — it does NOT
    /// mint a new durable row. The row keeps its id + `package_ref` + `display_name`
    /// (payload dir owns them), `attempts` climbs, the `wire_package_id` rotates to
    /// a fresh per-attempt id, every `sync_outbound_files` row resets to `pending`,
    /// and the journal records `resend` after the earlier `cancelled`. The loopback
    /// peer never starts an endpoint, so the resend's re-announce fails fast and the
    /// row parks non-terminal (never confirms) — exactly the surface this pins.
    #[tokio::test]
    async fn resend_resets_same_terminal_row_in_place() {
        let (tmp, ctx) = test_ctx();
        let pkg_dir = build_one_frame_package(&ctx, tmp.path());
        let ctx = Arc::new(ctx);
        let db_path = db(&ctx).unwrap().path().to_path_buf();

        let manifest = package::read_manifest(&pkg_dir).unwrap();
        assert!(
            !manifest.is_empty(),
            "fixture package has >=1 manifest record"
        );
        let files: Vec<crate::sharing::types::AnnounceFileEntry> = manifest
            .iter()
            .map(|r| crate::sharing::types::AnnounceFileEntry {
                rel_path: r.rel_path.clone(),
                byte_size: r.byte_size,
                frame_uuid: r.frame_uuid.clone(),
            })
            .collect();
        let display = "M42 — resend batch".to_string();

        let (sender, peer) = loopback_sender_for(&db_path).await;
        let collab_sender = Arc::new(SyncSenderRuntime::new());
        let sync = SyncRuntime::new();

        // Enqueue WITH a name + file rows, then cancel → terminal Cancelled.
        let (engine, _) = sender.current_for(&peer).await.unwrap();
        let id = engine
            .enqueue_package(
                &pkg_dir,
                Some(display.clone()),
                files.clone(),
                PackageLayout::Batch,
            )
            .await
            .unwrap();
        engine.cancel(id).await.unwrap();
        wait_outbound_state(&db_path, id, OutboundState::Cancelled).await;

        // Capture the pre-resend row: its wire id + attempts (the negotiate on the
        // first drive persisted a wire id even though the announce could not reach
        // the never-started peer).
        let before = {
            let store = CatalogSyncStore::open(&db_path).unwrap();
            store.get_outbound(id).unwrap().expect("row exists")
        };
        let old_wire = before.wire_package_id.clone();
        assert!(old_wire.is_some(), "the first attempt persisted a wire id");

        // Resend → the SAME id back, NOT a new row.
        let same_id = resend_transfer(&ctx, &sender, Arc::clone(&collab_sender), &sync, id, None)
            .await
            .unwrap();
        assert_eq!(
            same_id, id,
            "resend returns the SAME transfer id (no new row)"
        );

        // The reset ran synchronously inside `resend_transfer` before the async
        // re-drive; read it straight back.
        let after = {
            let store = CatalogSyncStore::open(&db_path).unwrap();
            store.get_outbound(id).unwrap().expect("row still exists")
        };
        assert!(
            !after.state.is_terminal(),
            "resend un-terminalizes the SAME row"
        );
        assert!(
            after.wire_package_id.is_some() && after.wire_package_id != old_wire,
            "resend rotates the per-attempt wire id (was {old_wire:?}, now {:?})",
            after.wire_package_id,
        );
        assert!(
            after.attempts >= before.attempts + 1,
            "resend increments the attempt counter ({} → {})",
            before.attempts,
            after.attempts,
        );
        assert_eq!(
            after.display_name.as_deref(),
            Some(display.as_str()),
            "the row keeps its display_name across the reset",
        );

        // Exactly ONE outbound row for this peer — no new row was minted.
        {
            let store = CatalogSyncStore::open(&db_path).unwrap();
            let rows = store.all_outbound(100).unwrap();
            assert_eq!(rows.len(), 1, "resend mints no new row, got {rows:?}");
        }

        // Every per-file row is back to `pending` with no settled outcome.
        let file_rows = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            list_outbound_files(&conn, id).unwrap()
        };
        assert_eq!(
            file_rows.len(),
            manifest.len(),
            "every manifest file has a row"
        );
        assert!(
            file_rows
                .iter()
                .all(|f| f.state == crate::sync::OutboundFileState::Pending && f.outcome.is_none()),
            "every file row reset to pending with no outcome, got {file_rows:?}",
        );

        // The journal records `cancelled` (from the cancel) then `resend`, in order.
        // `list_sync_events` is newest-first; reverse to chronological.
        let kinds: Vec<String> = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let mut k: Vec<String> = list_sync_events(&conn, Direction::Sent, &id.to_string())
                .unwrap()
                .into_iter()
                .map(|e| e.kind)
                .collect();
            k.reverse();
            k
        };
        let cancelled_at = kinds.iter().position(|k| k == "cancelled");
        let resend_at = kinds.iter().position(|k| k == "resend");
        assert!(
            cancelled_at.is_some(),
            "journal has a cancelled event, got {kinds:?}"
        );
        assert!(
            resend_at.is_some(),
            "journal has a resend event, got {kinds:?}"
        );
        assert!(
            cancelled_at < resend_at,
            "resend is journaled AFTER cancelled, got {kinds:?}",
        );
    }

    /// §D3: `resend_transfer` refuses a non-terminal (pending) package — only a
    /// `Failed` / `Cancelled` row is a resend candidate. Pins that the reset never
    /// fires on a live row (which would corrupt an in-flight transfer).
    #[tokio::test]
    async fn resend_rejects_non_terminal_package() {
        let (tmp, ctx) = test_ctx();
        let pkg_dir = build_one_frame_package(&ctx, tmp.path());
        let ctx = Arc::new(ctx);
        let db_path = db(&ctx).unwrap().path().to_path_buf();

        let (sender, peer) = loopback_sender_for(&db_path).await;
        let collab_sender = Arc::new(SyncSenderRuntime::new());
        let sync = SyncRuntime::new();

        // Enqueue but do NOT cancel — the row is non-terminal (Queued/Announced).
        let (engine, _) = sender.current_for(&peer).await.unwrap();
        let id = engine
            .enqueue_package(&pkg_dir, None, Vec::new(), PackageLayout::Batch)
            .await
            .unwrap();

        let err = resend_transfer(&ctx, &sender, collab_sender, &sync, id, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ApiError::Invalid(_)),
            "resend rejects a pending (non-terminal) package, got {err:?}",
        );
    }

    /// Task D: `resend_transfer` on a RECEIVER-DECLINED row (terminal `Cancelled`
    /// with `last_error = CANCELLED_BY_RECEIVER_DETAIL`) mints a NEW transfer instead
    /// of resetting the same row — the deliberate re-ask. Pins the mechanics:
    /// - a NEW `sync_outbound` id is returned (≠ the declined id);
    /// - the payload dir is renamed to a fresh uuid sibling (old path gone, new dir
    ///   present WITH its payload); the declined row keeps its now-stale `package_ref`;
    /// - the manifest (`sync_outbound_files`) is cloned onto the new row;
    /// - `sync_sources` retention is re-keyed onto the new dir (old ref → 0, new → N);
    /// - both journals carry the cross-link (`as_new_transfer=` / `of_declined=`);
    /// - a SECOND Resend of the OLD id now errors honestly (its payload dir is gone).
    ///
    /// The declined row is seeded DIRECTLY (not driven by the engine) so its
    /// `last_error` cannot be raced by an announce-failure re-stamp — a durable
    /// `Cancelled` + the exact receiver-decline reason mirrors the engine's
    /// all-cancelled ack terminal.
    #[tokio::test]
    async fn resend_of_declined_mints_new_transfer() {
        use crate::sharing::types::AnnounceFileEntry;
        use crate::sync::store::{insert_outbound_with_files, live_sources_for_package};

        let (tmp, ctx) = test_ctx();
        let pkg_dir = build_one_frame_package(&ctx, tmp.path());
        let old_ref = pkg_dir.to_string_lossy().to_string();
        let ctx = Arc::new(ctx);
        let db_path = db(&ctx).unwrap().path().to_path_buf();

        let manifest = package::read_manifest(&pkg_dir).unwrap();
        assert!(
            !manifest.is_empty(),
            "fixture package has >=1 manifest record"
        );
        let n = manifest.len();
        let files: Vec<AnnounceFileEntry> = manifest
            .iter()
            .map(|r| AnnounceFileEntry {
                rel_path: r.rel_path.clone(),
                byte_size: r.byte_size,
                frame_uuid: r.frame_uuid.clone(),
            })
            .collect();
        let display = "M42 — declined batch".to_string();

        let (sender, peer) = loopback_sender_for(&db_path).await;
        let collab_sender = Arc::new(SyncSenderRuntime::new());
        let sync = SyncRuntime::new();

        // Seed the declined row DIRECTLY under the loopback peer, then stamp the
        // durable `Cancelled` + receiver-decline reason.
        let old_id = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let id = insert_outbound_with_files(
                &conn,
                &old_ref,
                &node_id_hex(&peer),
                Some(&display),
                &files,
                // Mirror-hierarchy T2: seeded NON-default so the clone assertion
                // below cannot pass on the column's `batch` default.
                PackageLayout::Mirror,
            )
            .unwrap();
            conn.execute(
                "UPDATE sync_outbound SET state = 'cancelled', last_error = ?2 WHERE id = ?1",
                rusqlite::params![id, CANCELLED_BY_RECEIVER_DETAIL],
            )
            .unwrap();
            id
        };
        // The fixture package wrote one retention row per frame, keyed on old_ref.
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            assert_eq!(
                live_sources_for_package(&conn, &old_ref).unwrap().len(),
                n,
                "fixture seeded sync_sources on the old ref"
            );
        }

        // Resend → a NEW transfer.
        let new_id = resend_transfer(
            &ctx,
            &sender,
            Arc::clone(&collab_sender),
            &sync,
            old_id,
            None,
        )
        .await
        .unwrap();
        assert_ne!(
            new_id, old_id,
            "a receiver-declined resend mints a NEW transfer"
        );

        // The new row owns a fresh dir; the old dir was renamed away.
        let new_ref = {
            let store = CatalogSyncStore::open(&db_path).unwrap();
            store
                .get_outbound(new_id)
                .unwrap()
                .expect("new row exists")
                .package_ref
        };
        assert_ne!(new_ref, old_ref, "the new transfer has a fresh package_ref");

        // Mirror-hierarchy T2: the landing shape is CLONED onto the new transfer.
        // A decline is a receiver's "not this one", not a re-choice of where the
        // files go — resending under a different shape would silently relocate
        // them. Nothing else in the flow re-asks the user, so the stamp must ride
        // along with the manifest and the name.
        {
            let store = CatalogSyncStore::open(&db_path).unwrap();
            assert_eq!(
                store.get_outbound(new_id).unwrap().unwrap().layout,
                PackageLayout::Mirror,
                "the declined transfer's layout is cloned onto the new one"
            );
        }
        assert!(
            !pkg_dir.exists(),
            "the declined payload dir was renamed away"
        );
        let new_dir = PathBuf::from(&new_ref);
        assert!(new_dir.exists(), "the new payload dir is present");
        assert!(
            package_has_payload(&new_dir),
            "the new dir carries the manifest + payload"
        );

        // The declined row is kept as history — same id, still Cancelled + reason,
        // with its now-stale package_ref string.
        {
            let store = CatalogSyncStore::open(&db_path).unwrap();
            let old = store
                .get_outbound(old_id)
                .unwrap()
                .expect("declined row kept");
            assert_eq!(
                old.state,
                OutboundState::Cancelled,
                "declined row kept as history"
            );
            assert_eq!(
                old.last_error.as_deref(),
                Some(CANCELLED_BY_RECEIVER_DETAIL),
                "declined row keeps its receiver-decline reason"
            );
            assert_eq!(
                old.package_ref, old_ref,
                "declined row keeps its (now-stale) package_ref"
            );
        }

        // The manifest was cloned onto the new row (same rel_paths).
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let new_files = list_outbound_files(&conn, new_id).unwrap();
            assert_eq!(
                new_files.len(),
                n,
                "every manifest file cloned onto the new row"
            );
            let mut got: Vec<String> = new_files.iter().map(|f| f.rel_path.clone()).collect();
            let mut want: Vec<String> = files.iter().map(|f| f.rel_path.clone()).collect();
            got.sort();
            want.sort();
            assert_eq!(
                got, want,
                "cloned file rel_paths match the declined manifest"
            );
        }

        // Retention followed the payload: N on the new ref, 0 on the old.
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            assert_eq!(
                live_sources_for_package(&conn, &new_ref).unwrap().len(),
                n,
                "sync_sources re-keyed onto the new ref"
            );
            assert_eq!(
                live_sources_for_package(&conn, &old_ref).unwrap().len(),
                0,
                "no sync_sources remain on the old ref"
            );
        }

        // Both journals carry the cross-link.
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let old_details: Vec<String> =
                list_sync_events(&conn, Direction::Sent, &old_id.to_string())
                    .unwrap()
                    .into_iter()
                    .filter(|e| e.kind == "resend")
                    .filter_map(|e| e.detail)
                    .collect();
            assert!(
                old_details
                    .iter()
                    .any(|d| d == &format!("as_new_transfer={new_id}")),
                "old journal cross-links to the new id, got {old_details:?}"
            );
            let new_details: Vec<String> =
                list_sync_events(&conn, Direction::Sent, &new_id.to_string())
                    .unwrap()
                    .into_iter()
                    .filter(|e| e.kind == "resend")
                    .filter_map(|e| e.detail)
                    .collect();
            assert!(
                new_details
                    .iter()
                    .any(|d| d == &format!("of_declined={old_id}")),
                "new journal cross-links to the old id, got {new_details:?}"
            );
        }

        // A SECOND Resend of the OLD id now errors honestly — its payload is gone.
        let err = resend_transfer(&ctx, &sender, collab_sender, &sync, old_id, None)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ApiError::Invalid(m) if m.contains("missing on disk")),
            "a re-resend of the emptied declined row reports missing data, got {err:?}",
        );
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
                project: None,
                package_id: None,
                batch_name: None,
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
            project: None,
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

    // ── Per-batch file detail (task 14) ──────────────────────────────────────

    /// Task 14: after a real loopback confirm, `list_transfer_files(Sent, id)`
    /// returns one entry per manifest frame with the peer's ack verdict —
    /// `Some("ingested")` — recovered from the sender's OWN confirmed history via
    /// the package-dir-basename batch key (no receiver-side `sync_receipts` read).
    /// The manifest is the authoritative name/size source; sent entries carry no
    /// `bytes_done`.
    #[tokio::test]
    async fn list_transfer_files_sent_reports_ingested_after_loopback_confirm() {
        use crate::sharing::loopback::LoopbackNetwork;
        use crate::sharing::types::{FrameReceipt, ReceiptOutcome, TransportEvent};
        use crate::sharing::{noop_fetch_sink, SharingTransport};

        let (tmp, ctx) = test_ctx();
        let dir = tmp.path();
        let f1 = insert_fixture_frame(&ctx, dir, "light-0001.fits", "M42", false);
        let f2 = insert_fixture_frame(&ctx, dir, "light-0002.fits", "M42", false);

        // The sender engine writes to the catalog sync store (== ctx's DB), so
        // `list_transfer_files` reads the same rows it wrote.
        let (_sync_dir, db_path) = sync_paths(&ctx).unwrap();
        let pkg_root = tmp.path().join("packages");
        let pkg_dir = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            build_selection_package(
                &conn,
                "origin-dev",
                &pkg_root,
                selection_entries(&conn, &[f1, f2], None).unwrap(),
                None,
                None,
            )
            .unwrap()
            .pkg_dir
            .expect("a package was written")
        };

        // Reactive loopback receiver: fetch every announce and ack each frame
        // `Ingested`, driving the sender to Confirmed (+ its confirm history).
        let net = LoopbackNetwork::new();
        let receiver = Arc::new(net.endpoint());
        let receiver_id = receiver.start().await.unwrap().node_id;
        let recv_root = tmp.path().join("recv");
        {
            let receiver = receiver.clone();
            tokio::spawn(async move {
                let mut events = receiver.events().await;
                let mut n = 0usize;
                while let Some(event) = events.recv().await {
                    let TransportEvent::AnnounceReceived { from, announce, .. } = event else {
                        continue;
                    };
                    n += 1;
                    let dest = recv_root.join(format!("fetch-{n}"));
                    if receiver
                        .fetch(from, &announce, &dest, noop_fetch_sink())
                        .await
                        .is_ok()
                    {
                        if let Ok(records) = crate::package::read_manifest(&dest) {
                            let receipts: Vec<FrameReceipt> = records
                                .iter()
                                .map(|r| FrameReceipt {
                                    frame_uuid: r.frame_uuid.clone(),
                                    xxh3: r.xxh3.clone(),
                                    outcome: ReceiptOutcome::Ingested,
                                })
                                .collect();
                            let _ = receiver.ack(from, &announce.package_id, receipts).await;
                        }
                    }
                }
            });
        }

        let store = Arc::new(CatalogSyncStore::open(&db_path).unwrap());
        let engine = SyncEngine::spawn(
            Arc::clone(&store) as Arc<dyn SyncStore>,
            Arc::new(net.endpoint()) as Arc<dyn SharingTransport>,
            receiver_id,
        );
        let id = engine
            .enqueue_package(&pkg_dir, None, Vec::new(), PackageLayout::Batch)
            .await
            .unwrap();

        // Poll until confirmed (bounded).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let confirmed =
                store.get_outbound(id).unwrap().map(|r| r.state) == Some(OutboundState::Confirmed);
            if confirmed {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "package never confirmed"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let files = list_transfer_files(&ctx, Direction::Sent, id).unwrap();
        assert_eq!(files.len(), 2, "one entry per manifest frame");
        assert!(
            files
                .iter()
                .all(|f| f.outcome.as_deref() == Some("ingested")),
            "confirmed frames report the peer's ingested verdict: {files:?}"
        );
        assert!(
            files
                .iter()
                .any(|f| f.name == "light-0001.fits" && f.bytes_total > 0),
            "the manifest supplies name + bytes: {files:?}"
        );
        assert!(
            files.iter().all(|f| f.bytes_done.is_none()),
            "sent entries carry no bytes_done"
        );

        // tv2 follow-up: the confirmed row (a REAL engine terminal row, not a
        // seeded one) surfaces in `list_terminal_transfers().sent` so it survives
        // a restart — where the in-memory page ledger would not.
        let terminal = list_terminal_transfers(&ctx, None).unwrap();
        let confirmed = terminal
            .sent
            .iter()
            .find(|s| s.id == id)
            .expect("confirmed row in terminal sent");
        assert_eq!(confirmed.display_state, "confirmed");
        assert_eq!(
            confirmed.file_count, 2,
            "the confirmed batch's manifest enriches its terminal summary"
        );

        // Owner follow-up (dead Resend button): `resendable` reflects whether the
        // package payload is STILL on disk, not just the raw terminal state — a
        // cancelled row whose payload retention already swept must not offer a
        // Resend button that dead-ends in "data missing on disk". Two REAL
        // sibling package dirs (same `build_selection_package` this test already
        // uses, so `package_has_payload`'s manifest+payload check is genuine, not
        // a fake path): one left intact, one with its payload cleaned up
        // (simulated by deleting the dir, exactly what confirm/retention does).
        let pkg_root2 = tmp.path().join("packages2");
        let intact_dir = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            build_selection_package(
                &conn,
                "origin-dev",
                &pkg_root2,
                selection_entries(&conn, &[f1, f2], None).unwrap(),
                None,
                None,
            )
            .unwrap()
            .pkg_dir
            .expect("a package was written")
        };
        let swept_dir = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            build_selection_package(
                &conn,
                "origin-dev",
                &pkg_root2,
                selection_entries(&conn, &[f1, f2], None).unwrap(),
                None,
                None,
            )
            .unwrap()
            .pkg_dir
            .expect("a package was written")
        };
        std::fs::remove_dir_all(&swept_dir).unwrap();

        let peer_hex = node_id_hex(&receiver_id);
        let (intact_id, swept_id) = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let intact = crate::sync::store::insert_outbound_with_files(
                &conn,
                &intact_dir.to_string_lossy(),
                &peer_hex,
                Some("Cancelled — payload intact"),
                &[],
                PackageLayout::Batch,
            )
            .unwrap();
            let swept = crate::sync::store::insert_outbound_with_files(
                &conn,
                &swept_dir.to_string_lossy(),
                &peer_hex,
                Some("Cancelled — payload swept"),
                &[],
                PackageLayout::Batch,
            )
            .unwrap();
            (intact, swept)
        };
        set_outbound_terminal(&ctx, intact_id, "cancelled", "2026-07-20T09:00:00.000Z");
        set_outbound_terminal(&ctx, swept_id, "cancelled", "2026-07-20T09:30:00.000Z");

        let terminal2 = list_terminal_transfers(&ctx, None).unwrap();
        let intact = terminal2
            .sent
            .iter()
            .find(|s| s.id == intact_id)
            .expect("intact cancelled row");
        assert!(
            intact.resendable,
            "a cancelled row with its payload intact on disk is resendable"
        );
        let swept = terminal2
            .sent
            .iter()
            .find(|s| s.id == swept_id)
            .expect("swept cancelled row");
        assert!(
            !swept.resendable,
            "a cancelled row whose payload was cleaned up is NOT resendable"
        );

        engine.shutdown().await;
    }

    /// Task 14: an outbound package with NO confirm yet lists every manifest file
    /// with `outcome = None` (in flight), never an error — the manifest is the
    /// authoritative list even before any ack lands.
    #[test]
    fn list_transfer_files_sent_in_flight_lists_files_without_outcome() {
        let (tmp, ctx) = test_ctx();
        let dir = tmp.path();
        let f1 = insert_fixture_frame(&ctx, dir, "light-0001.fits", "M42", false);

        let (_sync_dir, db_path) = sync_paths(&ctx).unwrap();
        let pkg_root = tmp.path().join("packages");
        let (pkg_dir, id) = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let pkg_dir = build_selection_package(
                &conn,
                "origin-dev",
                &pkg_root,
                selection_entries(&conn, &[f1], None).unwrap(),
                None,
                None,
            )
            .unwrap()
            .pkg_dir
            .expect("a package was written");
            drop(conn);
            let store = CatalogSyncStore::open(&db_path).unwrap();
            let id = store
                .enqueue(
                    &pkg_dir.to_string_lossy(),
                    [9u8; 32],
                    None,
                    &[],
                    PackageLayout::Batch,
                )
                .unwrap();
            (pkg_dir, id)
        };
        let _ = pkg_dir;

        let files = list_transfer_files(&ctx, Direction::Sent, id).unwrap();
        assert_eq!(
            files.len(),
            1,
            "manifest lists the one frame even with no ack yet"
        );
        assert_eq!(files[0].name, "light-0001.fits");
        assert!(
            files[0].outcome.is_none(),
            "no verdict while the send is in flight"
        );
    }

    /// Task 14: a terminal inbound package lists its received frames from history
    /// (keyed by the wire package_id), each with the receiver's outcome and its
    /// received bytes. Resolved by the durable `sync_inbound.id`, not the wire id.
    #[test]
    fn list_transfer_files_received_terminal_reads_from_history() {
        use crate::sync::store::{set_inbound_state, upsert_inbound_announced};
        use crate::sync::InboundState;

        let (_tmp, ctx) = test_ctx();
        let pkg = "wire-pkg-77";
        let peer = "aa".repeat(32);
        let inbound_id = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let id = upsert_inbound_announced(&conn, &peer, pkg, 2, 300).unwrap();
            // Two received history rows for this batch, then drive the row terminal.
            for (uuid, name, bytes) in [("u-1", "a.fits", 100u64), ("u-2", "b.fits", 200u64)] {
                crate::sync::store::insert_history_row(
                    &conn,
                    &HistoryRow {
                        frame_uuid: uuid.into(),
                        filename: name.into(),
                        object: Some("M42".into()),
                        peer_device: peer.clone(),
                        direction: Direction::Received,
                        bytes,
                        started_at: "2026-07-15T00:00:00.000Z".into(),
                        finished_at: Some("2026-07-15T00:00:01.000Z".into()),
                        outcome: "ingested".into(),
                        project: None,
                        package_id: Some(pkg.into()),
                        batch_name: None,
                    },
                )
                .unwrap();
            }
            set_inbound_state(&conn, pkg, InboundState::Done, None).unwrap();
            id
        };

        let files = list_transfer_files(&ctx, Direction::Received, inbound_id).unwrap();
        assert_eq!(files.len(), 2, "one entry per received frame from history");
        assert!(files
            .iter()
            .all(|f| f.outcome.as_deref() == Some("ingested")));
        let a = files
            .iter()
            .find(|f| f.name == "a.fits")
            .expect("a.fits present");
        assert_eq!(a.bytes_total, 100);
        assert_eq!(
            a.bytes_done,
            Some(100),
            "terminal received: bytes_done reflects received bytes"
        );
    }

    /// Task 14: `list_transfer_files` surfaces a clean `NotFound` for an unknown
    /// row id in either direction (never a panic or silent empty).
    #[test]
    fn list_transfer_files_unknown_id_is_not_found() {
        let (_tmp, ctx) = test_ctx();
        assert!(matches!(
            list_transfer_files(&ctx, Direction::Sent, 4242),
            Err(ApiError::NotFound(_))
        ));
        assert!(matches!(
            list_transfer_files(&ctx, Direction::Received, 4242),
            Err(ApiError::NotFound(_))
        ));
    }

    // ── Terminal transfers survive restart (tv2 follow-up) ───────────────────
    //
    // Bug: `get_sync_status` returns only `non_terminal()` rows, so a
    // confirmed/failed/cancelled transfer only lingered in the in-memory page
    // ledger — it vanished on relaunch, taking its Resend button + detail.
    // `list_terminal_transfers` resurrects the recent terminal window from the
    // durable tables; the rows + their per-file rows persist regardless of any
    // in-memory ledger (the restart-survival contract).

    /// Directly stamp a terminal `state` + an explicit `created_at` (and, for
    /// `confirmed`, a matching `confirmed_at`) onto an outbound row — the
    /// persisted shape a real engine confirm/fail/cancel leaves behind, seeded
    /// without spinning up an engine. The explicit timestamp makes the
    /// newest-first ordering (`COALESCE(confirmed_at, created_at) DESC`)
    /// deterministic regardless of wall-clock granularity.
    fn set_outbound_terminal(ctx: &ServiceContext, id: i64, state: &str, ts: &str) {
        let db = db(ctx).unwrap();
        let conn = db.conn();
        let confirmed_at = if state == "confirmed" { Some(ts) } else { None };
        conn.execute(
            "UPDATE sync_outbound SET state = ?1, created_at = ?2, confirmed_at = ?3 WHERE id = ?4",
            rusqlite::params![state, ts, confirmed_at, id],
        )
        .unwrap();
    }

    /// tv2 follow-up: `list_terminal_transfers` returns ONLY terminal rows —
    /// outbound `confirmed|failed|cancelled`, inbound `done|failed|cancelled` —
    /// newest-first, honors the limit, and carries display_name + file_counts.
    /// A cancelled outbound row appears in `sent` with its per-file rows still
    /// listable by its id (the restart-survival contract, read from the DB with
    /// no in-memory ledger involved).
    #[test]
    fn list_terminal_transfers_returns_only_terminal_rows_newest_first() {
        use crate::sharing::types::AnnounceFileEntry;
        use crate::sync::store::{
            insert_outbound_with_files, set_inbound_state, upsert_inbound_announced,
        };
        use crate::sync::InboundState;

        let (_tmp, ctx) = test_ctx();
        let peer = "aa".repeat(32);

        // Seed outbound rows in a known order (ids ascending): a non-terminal
        // queued row that must be excluded, then confirmed → failed → cancelled.
        // The cancelled row carries a display_name + 2 per-file rows.
        let (queued_id, confirmed_id, failed_id, cancelled_id) = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let queued = insert_outbound_with_files(
                &conn,
                "/pkg/queued",
                &peer,
                Some("Queued batch"),
                &[],
                PackageLayout::Batch,
            )
            .unwrap();
            let confirmed = insert_outbound_with_files(
                &conn,
                "/pkg/confirmed",
                &peer,
                Some("Confirmed batch"),
                &[],
                PackageLayout::Batch,
            )
            .unwrap();
            let failed = insert_outbound_with_files(
                &conn,
                "/pkg/failed",
                &peer,
                Some("Failed batch"),
                &[],
                PackageLayout::Batch,
            )
            .unwrap();
            let files = [
                AnnounceFileEntry {
                    rel_path: "cam/a.fits".into(),
                    byte_size: 100,
                    frame_uuid: "u-a".into(),
                },
                AnnounceFileEntry {
                    rel_path: "cam/b.fits".into(),
                    byte_size: 200,
                    frame_uuid: "u-b".into(),
                },
            ];
            let cancelled = insert_outbound_with_files(
                &conn,
                "/pkg/cancelled",
                &peer,
                Some("Nightly batch"),
                &files,
                PackageLayout::Batch,
            )
            .unwrap();
            (queued, confirmed, failed, cancelled)
        };
        // Explicit, well-separated finished timestamps → deterministic
        // newest-first order: cancelled (latest) > failed > confirmed (earliest).
        set_outbound_terminal(&ctx, confirmed_id, "confirmed", "2026-07-20T10:00:00.000Z");
        set_outbound_terminal(&ctx, failed_id, "failed", "2026-07-20T11:00:00.000Z");
        set_outbound_terminal(&ctx, cancelled_id, "cancelled", "2026-07-20T12:00:00.000Z");

        // Seed inbound rows: an announced (non-terminal) one that must be
        // excluded, then done → failed → cancelled.
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            upsert_inbound_announced(&conn, &peer, "in-announced", 1, 10).unwrap();
            upsert_inbound_announced(&conn, &peer, "in-done", 1, 10).unwrap();
            set_inbound_state(&conn, "in-done", InboundState::Done, None).unwrap();
            upsert_inbound_announced(&conn, &peer, "in-failed", 1, 10).unwrap();
            set_inbound_state(&conn, "in-failed", InboundState::Failed, Some("boom")).unwrap();
            upsert_inbound_announced(&conn, &peer, "in-cancelled", 1, 10).unwrap();
            set_inbound_state(&conn, "in-cancelled", InboundState::Cancelled, None).unwrap();
        }

        let terminal = list_terminal_transfers(&ctx, None).unwrap();

        // Only terminal rows, newest-first (reverse insertion / finish order).
        let sent_ids: Vec<i64> = terminal.sent.iter().map(|s| s.id).collect();
        assert_eq!(
            sent_ids,
            vec![cancelled_id, failed_id, confirmed_id],
            "sent: terminal only, newest-first"
        );
        assert!(
            !sent_ids.contains(&queued_id),
            "a non-terminal queued row must be excluded"
        );

        let recv_states: Vec<&str> = terminal
            .received
            .iter()
            .map(|r| r.display_state.as_str())
            .collect();
        assert_eq!(
            recv_states,
            vec!["cancelled", "failed", "done"],
            "received: terminal only, newest-first"
        );
        assert!(
            !terminal
                .received
                .iter()
                .any(|r| r.package_id == "in-announced"),
            "a non-terminal announced inbound row must be excluded"
        );

        // The cancelled outbound summary carries its display_name + file_counts.
        let cancelled = terminal.sent.iter().find(|s| s.id == cancelled_id).unwrap();
        assert_eq!(cancelled.display_name.as_deref(), Some("Nightly batch"));
        assert_eq!(cancelled.display_state, "cancelled");
        assert_eq!(
            cancelled.file_counts.total, 2,
            "per-file rows counted into the summary"
        );

        // Limit is honored (newest row only).
        let one = list_terminal_transfers(&ctx, Some(1)).unwrap();
        assert_eq!(one.sent.len(), 1);
        assert_eq!(
            one.sent[0].id, cancelled_id,
            "limit keeps the newest terminal row"
        );
        assert_eq!(one.received.len(), 1);

        // Restart-survival: the cancelled row's per-file rows are still listable
        // by its durable id, read straight from the DB (no in-memory ledger).
        let files = list_transfer_files(&ctx, Direction::Sent, cancelled_id).unwrap();
        assert_eq!(
            files.len(),
            2,
            "the cancelled batch's per-file rows survive as detail"
        );
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
        assert!(
            !autostart_gate(false, false),
            "neither condition: must not start"
        );
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
        assert!(
            !account_signed_in(&ctx).unwrap(),
            "no identity yet -> not signed in"
        );

        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            crate::db::set_setting(&conn, keys::ACCOUNT_DEVICE_ID, "device-1").unwrap();
        }
        assert!(
            account_signed_in(&ctx).unwrap(),
            "identity present -> signed in"
        );

        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            crate::db::delete_setting(&conn, keys::ACCOUNT_DEVICE_ID).unwrap();
        }
        assert!(
            !account_signed_in(&ctx).unwrap(),
            "identity cleared (sign-out) -> not signed in"
        );
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
        let sync = Arc::new(SyncRuntime::new());

        let started = autostart_if_enabled(
            Arc::new(ctx),
            Arc::clone(&sync),
            Arc::new(SyncSenderRuntime::new()),
            Arc::new(SyncSenderRuntime::new()),
            Arc::new(crate::events::NullEmitter),
        )
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
        let sync = Arc::new(SyncRuntime::new());
        let started = autostart_if_enabled(
            Arc::new(ctx),
            Arc::clone(&sync),
            Arc::new(SyncSenderRuntime::new()),
            Arc::new(SyncSenderRuntime::new()),
            Arc::new(crate::events::NullEmitter),
        )
        .await;
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
        assert!(
            !allow_default_relays(true, true),
            "signed-in + dev flag: still refused"
        );
        assert!(
            !allow_default_relays(true, false),
            "signed-in, no dev flag: refused"
        );
        assert!(
            allow_default_relays(false, true),
            "signed-out + dev flag: pure dev/ticket mode allowed"
        );
        assert!(
            !allow_default_relays(false, false),
            "signed-out, no dev flag: refused"
        );
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
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "relays": ["https://relay1.example.org"]
                })),
            )
            .mount(&server)
            .await;

        let res = pairing::resolve_relays(Some((server.uri().as_str(), "tok")), &[]).await;
        assert_eq!(res.urls, vec!["https://relay1.example.org".to_string()]);
        assert!(res.fresh, "a live hub answer is fresh");

        // Signed in (creds present) AND the dev flag on: the Default fallback
        // must be refused regardless — composing exactly what `resolve_relay_mode`
        // does internally.
        let allow_default = allow_default_relays(true, true);
        assert!(
            !allow_default,
            "a signed-in device must never get the Default-relay opt-in"
        );

        let mode = pairing::relay_mode_for(&res.urls, allow_default).unwrap();
        assert!(
            matches!(mode, iroh::RelayMode::Custom(_)),
            "must use the hub's relay map, not iroh's public defaults, got {mode:?}"
        );
    }

    // ── T3: structured rel_path + batch name ─────────────────────────────────

    /// A minimal `File` for the pure rel_path/name helpers (no DB, no disk).
    fn file_at(path: &str, filename: &str) -> crate::models::File {
        crate::models::File {
            id: None,
            path: path.to_string(),
            filename: filename.to_string(),
            size: 0,
            modified_at: chrono::Utc::now(),
            format: crate::models::FileFormat::FITS,
            created_at: chrono::Utc::now(),
            content_hash: None,
            archived_in_operation: None,
            archive_zip_path: None,
            archive_path_in_zip: None,
            uuid: None,
            updated_at: None,
        }
    }

    /// `common_ancestor`: the deepest shared directory; disjoint filesystem roots
    /// collapse to `None` (the flat-fallback trigger).
    #[test]
    fn common_ancestor_finds_deepest_shared_dir() {
        let under_one = vec![
            PathBuf::from("/data/M31/lights/a.fits"),
            PathBuf::from("/data/M31/flats/b.fits"),
        ];
        assert_eq!(
            common_ancestor(&under_one),
            Some(PathBuf::from("/data/M31"))
        );

        // Same directory → that directory (rel_paths become flat basenames).
        let same = vec![
            PathBuf::from("/data/M31/a.fits"),
            PathBuf::from("/data/M31/b.fits"),
        ];
        assert_eq!(common_ancestor(&same), Some(PathBuf::from("/data/M31")));

        // Disjoint top-level roots → None.
        let disjoint = vec![
            PathBuf::from("/vol_a/x/a.fits"),
            PathBuf::from("/vol_b/y/b.fits"),
        ];
        assert_eq!(common_ancestor(&disjoint), None);
    }

    /// `dedup_in_dir`: collision-free names within one directory, suffixed before
    /// the extension; a different directory is an independent namespace.
    #[test]
    fn dedup_in_dir_suffixes_before_extension() {
        let mut used: HashMap<String, HashSet<String>> = HashMap::new();
        assert_eq!(dedup_in_dir("d", "a.fits", &mut used), "a.fits");
        assert_eq!(dedup_in_dir("d", "a.fits", &mut used), "a_2.fits");
        assert_eq!(dedup_in_dir("d", "a.fits", &mut used), "a_3.fits");
        // Extensionless.
        assert_eq!(dedup_in_dir("d", "readme", &mut used), "readme");
        assert_eq!(dedup_in_dir("d", "readme", &mut used), "readme_2");
        // A different directory does not collide.
        assert_eq!(dedup_in_dir("other", "a.fits", &mut used), "a.fits");
    }

    /// `source_rel_dir`: forward-slash dir between the ancestor and the file;
    /// empty when the file sits directly in the ancestor.
    #[test]
    fn source_rel_dir_is_forward_slash_relative() {
        let anc = Path::new("/data/M31");
        assert_eq!(
            source_rel_dir("/data/M31/lights/sub/a.fits", anc).as_deref(),
            Some("lights/sub")
        );
        assert_eq!(source_rel_dir("/data/M31/a.fits", anc).as_deref(), Some(""));
        assert_eq!(source_rel_dir("/elsewhere/a.fits", anc), None);
    }

    /// `assign_rel_path` source-relative: preserves the on-disk subdir under the
    /// ancestor, and a same-name file in the flat (no-ancestor) fallback is
    /// suffixed rather than flattened over.
    #[test]
    fn assign_rel_path_source_relative_and_flat_collision() {
        let layout = RelPathLayout::SourceRelative(Some(PathBuf::from("/data/M31")));
        let mut used: HashMap<String, HashSet<String>> = HashMap::new();
        assert_eq!(
            assign_rel_path(
                &layout,
                1,
                &file_at("/data/M31/lights/a.fits", "a.fits"),
                &mut used
            ),
            "lights/a.fits"
        );
        assert_eq!(
            assign_rel_path(
                &layout,
                2,
                &file_at("/data/M31/b.fits", "b.fits"),
                &mut used
            ),
            "b.fits"
        );

        // Flat fallback (disjoint roots): a repeated basename gets `_2`.
        let flat = RelPathLayout::SourceRelative(None);
        let mut used2: HashMap<String, HashSet<String>> = HashMap::new();
        assert_eq!(
            assign_rel_path(
                &flat,
                1,
                &file_at("/vol_a/x/dup.fits", "dup.fits"),
                &mut used2
            ),
            "dup.fits"
        );
        assert_eq!(
            assign_rel_path(
                &flat,
                2,
                &file_at("/vol_b/y/dup.fits", "dup.fits"),
                &mut used2
            ),
            "dup_2.fits"
        );
    }

    /// `assign_rel_path` WBPP: two frames sharing a rel_dir and basename collide,
    /// so the second is suffixed inside that subdir (never flattened out).
    #[test]
    fn assign_rel_path_wbpp_subdir_collision() {
        let mut map = HashMap::new();
        map.insert(1i64, "camera_asi/lights".to_string());
        map.insert(2i64, "camera_asi/lights".to_string());
        let layout = RelPathLayout::Wbpp(map);
        let mut used: HashMap<String, HashSet<String>> = HashMap::new();
        assert_eq!(
            assign_rel_path(&layout, 1, &file_at("/a/x.fits", "x.fits"), &mut used),
            "camera_asi/lights/x.fits"
        );
        assert_eq!(
            assign_rel_path(&layout, 2, &file_at("/b/x.fits", "x.fits"), &mut used),
            "camera_asi/lights/x_2.fits"
        );
    }

    /// Builder over payload entries: kinds, rel_paths and the retention linkage
    /// rule (spec 2026-08-28 §3 step 5 / D5).
    #[test]
    fn build_selection_package_honours_payload_kinds() {
        use crate::db::schema::init_db;
        use crate::fits_writer::write_fits_f32;
        use crate::package::PayloadKind;
        let tmp = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // One raw light (frame 10), one master (frame 20), one calibrated
        // artifact of frame 10 that is NOT in the catalog.
        let light = tmp.path().join("L_10.fits");
        let master = tmp.path().join("master_dark.fits");
        let artifact = tmp.path().join("c_L_10.fits");
        for p in [&light, &master, &artifact] {
            write_fits_f32(p, 4, 4, 1, &[1.0f32; 16], &[]).unwrap();
        }
        let insert = |file_id: i64,
                      frame_id: i64,
                      path: &std::path::Path,
                      imagetyp: &str,
                      is_master: i64| {
            let meta = std::fs::metadata(path).unwrap();
            let mtime: chrono::DateTime<chrono::Utc> = meta.modified().unwrap().into();
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, ?4, ?5, 'FITS')",
                rusqlite::params![file_id, path.to_string_lossy(), path.file_name().unwrap().to_string_lossy(), meta.len() as i64, mtime.to_rfc3339()],
            ).unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp, is_master, uuid) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![frame_id, file_id, imagetyp, is_master, format!("uuid-{frame_id}")],
            ).unwrap();
        };
        insert(1, 10, &light, "Light", 0);
        insert(2, 20, &master, "MasterDark", 1);

        let input = SelectionInput {
            entries: vec![
                PayloadEntry {
                    frame_id: 10,
                    source_path: light.clone(),
                    rel_path: "camera_X/lights/L_10.fits".into(),
                    kind: PayloadKind::RawFrame,
                },
                PayloadEntry {
                    frame_id: 20,
                    source_path: master.clone(),
                    rel_path: "camera_X/DARKS_5/master_dark.fits".into(),
                    kind: PayloadKind::Master,
                },
                PayloadEntry {
                    frame_id: 10,
                    source_path: artifact.clone(),
                    rel_path: "camera_X/lights/c_L_10.fits".into(),
                    kind: PayloadKind::CalibratedLight,
                },
            ],
            ineligible: Vec::new(),
            ancestor: None,
            total: 3,
        };
        let packages = tmp.path().join("packages");
        let built = build_selection_package(
            &conn,
            "ab".repeat(32).as_str(),
            &packages,
            input,
            Some("Test batch"),
            None,
        )
        .unwrap();
        assert_eq!(built.eligible.len(), 3);
        assert!(built.ineligible.is_empty());
        let dir = built.pkg_dir.unwrap();
        let records = crate::package::read_manifest(&dir).unwrap();
        let by_rel: std::collections::HashMap<_, _> =
            records.iter().map(|r| (r.rel_path.clone(), r)).collect();
        assert_eq!(
            by_rel["camera_X/lights/L_10.fits"].payload_kind,
            PayloadKind::RawFrame
        );
        assert_eq!(by_rel["camera_X/lights/L_10.fits"].frame_uuid, "uuid-10");
        assert_eq!(
            by_rel["camera_X/DARKS_5/master_dark.fits"].payload_kind,
            PayloadKind::Master
        );
        let cal = by_rel["camera_X/lights/c_L_10.fits"];
        assert_eq!(cal.payload_kind, PayloadKind::CalibratedLight);
        assert_ne!(
            cal.frame_uuid, "uuid-10",
            "artifact carries its own identity"
        );
        assert_eq!(
            cal.frame_meta["uuid"], "uuid-10",
            "frame_meta is the SOURCE light's snapshot"
        );
        assert!(cal.analysis.is_none());
        // Retention linkage: only the raw light.
        let linked: Vec<i64> = conn
            .prepare("SELECT file_id FROM sync_sources ORDER BY file_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            linked,
            vec![1],
            "masters and artifacts never enter sync_sources"
        );
        // strong_hash banked for catalog files only.
        let banked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE strong_hash IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(banked, 2);
    }

    /// `selection_entries`' kind classification — the one behavior change on the
    /// otherwise behavior-preserving selection path, and the gate on spec §D5
    /// ("masters are never reclaimable by retention through a send"). The signal
    /// is the CATALOG's `frames.is_master`, which the scanner sets from
    /// `imagetyp.is_master() || filename_is_master` (`fits_parser/mod.rs`) — a
    /// header-only test would miss a `master_dark_*.fits` still carrying
    /// `IMAGETYP = Dark` and ship it as a reclaimable `RawFrame`.
    #[test]
    fn selection_entries_labels_masters_by_the_catalog_signal() {
        use crate::db::schema::init_db;
        use crate::fits_writer::write_fits_f32;
        use crate::package::PayloadKind;
        let tmp = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let light = tmp.path().join("L_10.fits");
        let header_master = tmp.path().join("master_bias.fits");
        // The leaky shape: named like a master, header still says a plain Dark,
        // so only `frames.is_master` gives it away.
        let filename_master = tmp.path().join("master_dark_300s.fits");
        for p in [&light, &header_master, &filename_master] {
            write_fits_f32(p, 4, 4, 1, &[1.0f32; 16], &[]).unwrap();
        }
        let insert = |file_id: i64,
                      frame_id: i64,
                      path: &std::path::Path,
                      imagetyp: &str,
                      is_master: i64| {
            let meta = std::fs::metadata(path).unwrap();
            let mtime: chrono::DateTime<chrono::Utc> = meta.modified().unwrap().into();
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, ?4, ?5, 'FITS')",
                rusqlite::params![file_id, path.to_string_lossy(), path.file_name().unwrap().to_string_lossy(), meta.len() as i64, mtime.to_rfc3339()],
            ).unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp, is_master, uuid) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![frame_id, file_id, imagetyp, is_master, format!("uuid-{frame_id}")],
            ).unwrap();
        };
        insert(1, 10, &light, "Light", 0);
        insert(2, 20, &header_master, "MasterBias", 1);
        insert(3, 30, &filename_master, "Dark", 1);

        let input = selection_entries(&conn, &[10, 20, 30], None).unwrap();
        assert_eq!(input.total, 3);
        assert!(input.ineligible.is_empty());
        let kinds: HashMap<i64, PayloadKind> = input
            .entries
            .iter()
            .map(|e| (e.frame_id, e.kind.clone()))
            .collect();
        assert_eq!(kinds[&10], PayloadKind::RawFrame);
        assert_eq!(kinds[&20], PayloadKind::Master, "IMAGETYP says master");
        assert_eq!(
            kinds[&30],
            PayloadKind::Master,
            "filename-detected master: frames.is_master is the catalog's own signal"
        );

        // The consequence that matters: neither master may leave behind a
        // retention linkage the desktop could later reclaim (§D5).
        let built = build_selection_package(
            &conn,
            "ab".repeat(32).as_str(),
            &tmp.path().join("packages"),
            input,
            None,
            None,
        )
        .unwrap();
        assert_eq!(built.eligible.len(), 3);
        let linked: Vec<i64> = conn
            .prepare("SELECT file_id FROM sync_sources ORDER BY file_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(linked, vec![1], "only the raw light is reclaimable");
    }

    /// `resolve_batch_name` — each of the three auto-name rules plus the verbatim
    /// user override.
    #[test]
    fn resolve_batch_name_all_rules() {
        let (_tmp, ctx) = test_ctx();
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        // A frame set to name after.
        conn.execute(
            "INSERT INTO frames_set (id, name) VALUES (7, 'Andromeda Core')",
            [],
        )
        .unwrap();

        // 1) User-provided non-blank name is kept verbatim (whitespace preserved).
        assert_eq!(
            resolve_batch_name(Some("  My Send  "), &conn, Some(7), None, 3),
            "  My Send  "
        );
        // A blank user name falls through to auto.
        assert_eq!(
            resolve_batch_name(Some("   "), &conn, Some(7), None, 3),
            "Andromeda Core"
        );
        // 2) Object send → frame-set name.
        assert_eq!(
            resolve_batch_name(None, &conn, Some(7), None, 3),
            "Andromeda Core"
        );
        // 3) Browser folder → common-ancestor basename.
        assert_eq!(
            resolve_batch_name(None, &conn, None, Some(Path::new("/data/M31_lights")), 4),
            "M31_lights"
        );
        // 4) Loose files → "N files — YYYY-MM-DD HH:MM".
        let loose = resolve_batch_name(None, &conn, None, None, 5);
        assert!(loose.starts_with("5 files — "), "got {loose}");
        assert!(
            regex_like_timestamp(&loose),
            "auto name must carry a YYYY-MM-DD HH:MM stamp: {loose}"
        );
    }

    /// Loose check that the tail of an auto name is a `YYYY-MM-DD HH:MM` stamp
    /// (no regex dep): digits/dashes/colon in the expected shape.
    fn regex_like_timestamp(s: &str) -> bool {
        let tail = s.rsplit("— ").next().unwrap_or("");
        let b = tail.as_bytes();
        b.len() == 16
            && b[4] == b'-'
            && b[7] == b'-'
            && b[10] == b' '
            && b[13] == b':'
            && b.iter()
                .enumerate()
                .filter(|(i, _)| ![4usize, 7, 10, 13].contains(i))
                .all(|(_, c)| c.is_ascii_digit())
    }

    /// Source-relative browser send: files under a common dir keep their on-disk
    /// subdir structure in `rel_path`; the batch auto-name is the ancestor
    /// basename; and one `sync_outbound_files` row lands per manifest record with
    /// `state = pending`, `bytes_done = 0`.
    #[tokio::test]
    async fn source_relative_enqueue_writes_files_and_name() {
        use crate::sharing::loopback::LoopbackNetwork;
        use crate::sharing::SharingTransport;

        let (tmp, ctx) = test_ctx();
        let ctx = Arc::new(ctx);
        // Two files under a common "M31_data" dir, in different subdirs.
        let root = tmp.path().join("M31_data");
        let f1 = {
            let p = root.join("lights");
            std::fs::create_dir_all(&p).unwrap();
            insert_fixture_frame_at(&ctx, &p.join("light1.fits"), "M31")
        };
        let f2 = {
            let p = root.join("flats");
            std::fs::create_dir_all(&p).unwrap();
            insert_fixture_frame_at(&ctx, &p.join("flat1.fits"), "M31")
        };

        let db_path = db(&ctx).unwrap().path().to_path_buf();
        let net = LoopbackNetwork::new();
        let store = Arc::new(CatalogSyncStore::open(&db_path).unwrap());
        let engine = SyncEngine::spawn(
            store as Arc<dyn SyncStore>,
            Arc::new(net.endpoint()) as Arc<dyn SharingTransport>,
            [9u8; 32],
        );

        let pkg_root = tmp.path().join("packages");
        let result = build_and_enqueue_selection(
            &ctx,
            &engine,
            "origin-dev",
            &pkg_root,
            &[f1, f2],
            None, // auto-name → common-ancestor basename "M31_data"
            None, // browser send (no frame set)
        )
        .await
        .unwrap();
        assert_eq!(result.enqueued_count, 2);

        // Inspect the row the engine minted.
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let rows = crate::sync::store::all_outbound_rows(&conn, 10).unwrap();
        let row = rows.first().expect("an outbound row exists");
        assert_eq!(
            row.display_name.as_deref(),
            Some("M31_data"),
            "auto name = ancestor basename"
        );

        let files = crate::sync::store::list_outbound_files(&conn, row.id).unwrap();
        assert_eq!(files.len(), 2, "one row per manifest record");
        let rel: HashSet<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        assert!(
            rel.contains("lights/light1.fits"),
            "subdir preserved: {rel:?}"
        );
        assert!(
            rel.contains("flats/flat1.fits"),
            "subdir preserved: {rel:?}"
        );
        for f in &files {
            assert_eq!(f.state, crate::sync::OutboundFileState::Pending);
            assert_eq!(f.bytes_done, 0);
            assert!(f.byte_size > 0, "byte_size carried from the manifest");
        }
    }

    /// A user-provided batch name is stored verbatim on the outbound row.
    #[tokio::test]
    async fn user_batch_name_is_persisted_verbatim() {
        use crate::sharing::loopback::LoopbackNetwork;
        use crate::sharing::SharingTransport;

        let (tmp, ctx) = test_ctx();
        let ctx = Arc::new(ctx);
        let f1 = insert_fixture_frame_at(&ctx, &tmp.path().join("a.fits"), "M42");

        let db_path = db(&ctx).unwrap().path().to_path_buf();
        let net = LoopbackNetwork::new();
        let store = Arc::new(CatalogSyncStore::open(&db_path).unwrap());
        let engine = SyncEngine::spawn(
            store as Arc<dyn SyncStore>,
            Arc::new(net.endpoint()) as Arc<dyn SharingTransport>,
            [3u8; 32],
        );

        let pkg_root = tmp.path().join("packages");
        let name = Some("Trip to M42".to_string());
        build_and_enqueue_selection(
            &ctx,
            &engine,
            "origin-dev",
            &pkg_root,
            &[f1],
            name.as_deref(),
            None,
        )
        .await
        .unwrap();

        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let rows = crate::sync::store::all_outbound_rows(&conn, 10).unwrap();
        assert_eq!(
            rows.first().unwrap().display_name.as_deref(),
            Some("Trip to M42")
        );
    }

    /// Write a fixture file at an ABSOLUTE path (creating parent dirs) + its
    /// `files`/`frames` rows; returns the frame id. Sibling of
    /// `insert_fixture_frame` for tests that need real subdirectories.
    fn insert_fixture_frame_at(ctx: &ServiceContext, abs_path: &Path, object: &str) -> i64 {
        use crate::models::{File, FileFormat, Frame};
        if let Some(parent) = abs_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(
            abs_path,
            format!("payload-{}", abs_path.display()).as_bytes(),
        )
        .unwrap();
        let size = std::fs::metadata(abs_path).unwrap().len() as i64;
        let filename = abs_path.file_name().unwrap().to_string_lossy().into_owned();

        let db = db(ctx).unwrap();
        let conn = db.conn();
        let file = File {
            id: None,
            path: abs_path.to_string_lossy().into_owned(),
            filename,
            size,
            modified_at: chrono::Utc::now(),
            format: FileFormat::FITS,
            created_at: chrono::Utc::now(),
            content_hash: None,
            archived_in_operation: None,
            archive_zip_path: None,
            archive_path_in_zip: None,
            uuid: None,
            updated_at: None,
        };
        let file_id = crate::db::insert_file(&conn, &file).unwrap();
        let frame = Frame {
            file_id,
            object: Some(object.to_string()),
            ..Default::default()
        };
        crate::db::insert_frame(&conn, &frame).unwrap()
    }

    /// W1 §T1.3: the upload-limit bounds check, pinned through the REAL guard
    /// (`validate_upload_limit`) rather than a test-local copy — the
    /// ctx-taking `set_sync_upload_limit` only forwards to it. `0` is the
    /// unlimited sentinel and always passes; anything else must clear the
    /// 100 KB/s floor, below which a transfer stops looking slow and starts
    /// looking dead.
    #[test]
    fn validate_upload_limit_rejects_sub_floor_nonzero() {
        assert!(validate_upload_limit(0).is_ok(), "0 = unlimited");
        assert!(matches!(
            validate_upload_limit(1),
            Err(ApiError::Invalid(_))
        ));
        assert!(matches!(
            validate_upload_limit(99_999),
            Err(ApiError::Invalid(_))
        ));
        assert!(validate_upload_limit(100_000).is_ok(), "the floor itself");
        assert!(validate_upload_limit(5_000_000).is_ok());
    }

    /// W2 §T2.7: the receive-concurrency bounds check, pinned through the REAL
    /// guard (`validate_max_concurrent_receives`) — the ctx-taking setter only
    /// forwards to it. `0` would park every inbound transfer forever; above 8
    /// the lanes fight over one disk with nothing to gain.
    #[test]
    fn validate_max_concurrent_receives_rejects_out_of_range() {
        assert!(matches!(
            validate_max_concurrent_receives(0),
            Err(ApiError::Invalid(_))
        ));
        assert!(validate_max_concurrent_receives(1).is_ok(), "the floor");
        assert!(validate_max_concurrent_receives(8).is_ok(), "the ceiling");
        assert!(matches!(
            validate_max_concurrent_receives(9),
            Err(ApiError::Invalid(_))
        ));
    }

    /// The STARTUP half of W2 §T2.7: the value `receiver_hooks` carries into
    /// `ensure_started`, read through the REAL reader. A fresh install reports
    /// the shipped default; a persisted value overrides it. (The carrier's other
    /// end — `Option<usize>` → gate — is pinned by `apply_receive_limit_*` in
    /// `sync::receiver`; the two together cover the startup path without a bound
    /// iroh node.)
    #[test]
    fn configured_max_concurrent_receives_reads_the_persisted_value() {
        let (_tmp, ctx) = test_ctx();

        assert_eq!(
            configured_max_concurrent_receives(&ctx),
            Some(crate::sync::DEFAULT_MAX_CONCURRENT_RECEIVES),
            "a fresh install starts the receiver at the shipped default"
        );

        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            ctx.settings
                .persist_setting(&conn, keys::SYNC_MAX_CONCURRENT_RECEIVES, "5")
                .unwrap();
        }
        assert_eq!(
            configured_max_concurrent_receives(&ctx),
            Some(5),
            "a receiver started fresh picks the persisted cap up"
        );
    }

    /// The LIVE half of W2 §T2.7: with a receiver already started, the setter
    /// resizes its running gate — no restart. The persisted row and the live
    /// gate must both move.
    #[tokio::test]
    async fn set_max_concurrent_receives_resizes_a_running_receivers_gate() {
        let (tmp, ctx) = test_ctx();
        let (_ep, runtime) = started_loopback_runtime(&tmp).await;

        let control = runtime
            .inbound_control()
            .await
            .expect("a started runtime exposes its inbound control");
        assert_eq!(
            control.receive_gate.limit(),
            crate::sync::DEFAULT_MAX_CONCURRENT_RECEIVES,
            "the running receiver starts at the shipped default"
        );

        set_sync_max_concurrent_receives(&ctx, &runtime, 4)
            .await
            .expect("setter accepts an in-range value");

        assert_eq!(
            control.receive_gate.limit(),
            4,
            "the change lands on the RUNNING receiver's gate, not just the DB"
        );
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        assert_eq!(
            ctx.settings
                .get_sync_max_concurrent_receives(&conn)
                .unwrap(),
            4,
            "and it is persisted, so the next start applies it too"
        );
    }

    /// A rejected value changes NOTHING — not the row, not the live gate.
    #[tokio::test]
    async fn set_max_concurrent_receives_rejects_without_touching_gate_or_row() {
        let (tmp, ctx) = test_ctx();
        let (_ep, runtime) = started_loopback_runtime(&tmp).await;
        let control = runtime.inbound_control().await.unwrap();

        assert!(matches!(
            set_sync_max_concurrent_receives(&ctx, &runtime, 0).await,
            Err(ApiError::Invalid(_))
        ));
        assert_eq!(
            control.receive_gate.limit(),
            crate::sync::DEFAULT_MAX_CONCURRENT_RECEIVES,
            "a refused value never reaches the gate"
        );
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        assert_eq!(
            ctx.settings
                .get_sync_max_concurrent_receives(&conn)
                .unwrap(),
            crate::sync::DEFAULT_MAX_CONCURRENT_RECEIVES,
            "nor the row"
        );
    }

    /// An un-started receiver is not an error: the value is persisted and the
    /// eventual start applies it (the startup half above).
    #[tokio::test]
    async fn set_max_concurrent_receives_persists_without_a_started_receiver() {
        let (_tmp, ctx) = test_ctx();
        let runtime = SyncRuntime::new();
        assert!(runtime.inbound_control().await.is_none(), "not started");

        set_sync_max_concurrent_receives(&ctx, &runtime, 3)
            .await
            .expect("no receiver is not a failure");

        assert_eq!(
            configured_max_concurrent_receives(&ctx),
            Some(3),
            "the next start reads it back"
        );
    }
}
