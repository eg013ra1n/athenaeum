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
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::api::{db, ApiError};
use crate::events::ProgressEmitter;
use crate::package::{self, ManifestRecord, PayloadKind, MANIFEST_VERSION};
use crate::services::ServiceContext;
use crate::settings::{defaults, keys};
use crate::sharing::iroh::node::{RelayResolver, Role, SharedIrohNode};
use crate::sharing::types::NodeId;
use crate::sharing::SharingTransport;
use crate::sync::status::outbound_display_state;
use crate::sync::store::{
    get_inbound_by_row_id, inbound_active, inbound_file_counts, list_inbound_files,
    list_outbound_files, list_sync_events, outbound_file_counts, outbound_row_by_id,
    search_history_rows, terminal_inbound, terminal_outbound,
};
use crate::sync::{
    node_id_hex, pairing, CatalogSyncStore, Direction, HistoryQuery, HistoryRow, InboundRow,
    InboundSummary, OutboundRow, OutboundState, OutboundSummary, RefusalRefresher, StartedSender,
    SyncEngine, SyncEngineHandle, SyncReceiverStatus, SyncRuntime, SyncSenderRuntime,
    SyncSenderStatus, SyncStatus, SyncStore, TransferFileCounts, TransferFileEntry, TransportHealth,
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
    let v = ctx
        .settings
        .get_with_precedence(&conn, keys::SYNC_DEV_TICKET_PAIRING, defaults::SYNC_DEV_TICKET_PAIRING)?;
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

/// A node-id-hex → current-device-name map over the account device list — the
/// cached source the receiver reads to name an incoming sender's landing folder
/// by that sender's CURRENT friendly name (with no per-package hub round-trip).
/// Undecodable pubkeys are skipped; the same map `get_sync_device_names` builds,
/// but persisted so the receiver can resolve offline. Serialized to
/// [`SYNC_DEVICE_NAMES`](keys::SYNC_DEVICE_NAMES) by [`refresh_authorized_peers`].
fn account_device_names(
    devices: &[crate::account::AccountDevice],
) -> HashMap<String, String> {
    devices
        .iter()
        .filter_map(|d| pubkey_b64_to_hex(&d.pubkey).map(|hex| (hex, d.name.clone())))
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
fn project_announce_gate(ctx: &ServiceContext) -> Result<crate::sync::ProjectAnnounceGate, ApiError> {
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
    })
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
/// fired after a project package ingests + acks: a best-effort report-have so the
/// hub adds this device to the package's swarm. `tokio::spawn`ed off the receive
/// loop; a failure only means the hub doesn't list us as a holder yet.
fn on_project_ingested_hook(ctx: Arc<ServiceContext>) -> crate::sync::ProjectIngestedHook {
    Arc::new(move |project_id: String, package_id: String| {
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
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
    Arc::new(move |from: NodeId, project_id: String, package_id: String| {
        let ctx = Arc::clone(&ctx);
        let sender = Arc::clone(&collab_sender);
        let emitter = Arc::clone(&emitter);
        tokio::spawn(async move {
            if let Err(e) = crate::api::collab_exchange::handle_project_request(
                &ctx, &sender, from, project_id, package_id, Some(emitter),
            )
            .await
            {
                tracing::error!(error = %format!("{e:#}"), "collab request-to-serve failed");
            }
        });
    })
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
    if let Ok(db) = db(ctx) {
        let conn = db.conn();
        if let Err(e) = crate::db::set_setting(&conn, keys::SYNC_AUTHORIZED_PEERS, &hexes.join("\n"))
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
    tracing::debug!(interval_secs = PEERS_REFRESH_INTERVAL.as_secs(), "authorized-peers refresh timer installed");
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
pub(crate) async fn resolve_relay_mode(ctx: &ServiceContext) -> Result<(iroh::RelayMode, Vec<String>), ApiError> {
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
    node: &Arc<SharedIrohNode>,
    sync_sender: Arc<SyncSenderRuntime>,
    collab_sender: Arc<SyncSenderRuntime>,
) {
    node.set_wake_hook(Arc::new(move || {
        let sync_sender = Arc::clone(&sync_sender);
        let collab_sender = Arc::clone(&collab_sender);
        tokio::spawn(async move {
            sync_sender.kick_all().await;
            collab_sender.kick_all().await;
        });
    }));
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
                Ok(devices) => devices.into_iter().find_map(|d| {
                    match pairing::node_id_from_pubkey_b64(&d.pubkey) {
                        Ok(id) if id == peer => Some(d.endpoint_addr),
                        _ => None,
                    }
                }).flatten(),
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
pub(crate) async fn ensure_iroh_node(ctx: &ServiceContext) -> Result<Arc<SharedIrohNode>, ApiError> {
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
    install_node_wake_hook(&node, Arc::clone(&sync_sender), Arc::clone(&collab_sender));
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
    // Clone the emitter for the resurrection spawn before `ensure_started` moves it.
    let emitter_for_resurrect = Arc::clone(&emitter);
    sync.ensure_started(node, sync_dir, db_path, incoming, authorized, hooks, emitter)
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
    Ok(true)
}

/// The pure condition [`autostart_if_enabled`] gates on: the dev pairing flag,
/// OR this device being signed in (any Athenaeum node — no role gate). Factored
/// out for a fast, network-free unit test of the exact matrix (task A7
/// fix-review, broadened in sync Phase 2A).
fn autostart_gate(dev: bool, signed_in: bool) -> bool {
    dev || signed_in
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
    install_node_wake_hook(&node, Arc::clone(&sync_sender), Arc::clone(&collab_sender));
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

    // Clone the emitter for the resurrection spawn before `ensure_started` moves it.
    let emitter_for_resurrect = Arc::clone(&emitter);
    let ticket = sync
        .ensure_started(node, sync_dir, db_path, incoming, authorized, hooks, emitter)
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

/// Map one persisted `sync_outbound` row to its display [`OutboundSummary`] —
/// the SINGLE outbound-row → summary mapping, shared by the live Active-tab
/// rollup ([`build_sender_status`]) and the terminal-rows read
/// ([`list_terminal_transfers`]) so a package presents identically
/// (display_state, display_name, device_name, file_counts, retrying) whether it
/// came from the live poll or the restart-survival read. `device_names` /
/// `file_counts` are the batch-resolved maps the caller reads once; `now` anchors
/// the `waiting`-backoff derivation.
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
) -> OutboundSummary {
    let (file_count, byte_size) = package_totals(Path::new(&row.package_ref));
    let peer_hex = node_id_hex(&row.peer);
    let display_state = outbound_display_state(row.state, row.next_retry_at.as_deref(), now);
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
/// ([`list_terminal_transfers`]). Inbound presentation state mirrors the raw
/// state (no receiver-side retry/backoff concept yet), so `stalled_until` is
/// always `None`.
fn inbound_summary(
    row: InboundRow,
    device_names: &HashMap<String, String>,
    file_counts: &HashMap<i64, TransferFileCounts>,
) -> InboundSummary {
    // The stable per-transfer key the receiver keys its one long-lived row on;
    // fall back to the wire package id for a legacy row whose `batch_uuid` is
    // NULL (a v1/v2 receive, or a row created before the column).
    let batch_uuid = row.batch_uuid.clone().unwrap_or_else(|| row.package_id.clone());
    InboundSummary {
        id: row.id,
        package_id: row.package_id.clone(),
        package_short: short_id(&row.package_id),
        peer_short: short_id(&row.peer),
        display_state: row.state.as_str().to_string(),
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
    }
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
        .map(|row| outbound_summary(row, now, &device_names, &file_counts))
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
fn active_inbound_summaries(ctx: &ServiceContext) -> Result<Vec<InboundSummary>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let rows = inbound_active(&conn).map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    // ONE grouped per-file-counts query for the whole active set (§D5), plus the
    // device-name cache (one settings read, no hub).
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let file_counts = inbound_file_counts(&conn, &ids).map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    let device_names = cached_device_names(&conn);
    Ok(rows
        .into_iter()
        .map(|row| inbound_summary(row, &device_names, &file_counts))
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
    let active_inbound = active_inbound_summaries(ctx)?;
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
            let mut summary = outbound_summary(row, now, &device_names, &out_counts);
            summary.resendable = resendable;
            summary
        })
        .collect();
    let received = in_rows
        .into_iter()
        .map(|row| inbound_summary(row, &device_names, &in_counts))
        .collect();
    Ok(TerminalTransfers { sent, received })
}

/// The transfer history (received + sent), newest first.
pub fn list_history(ctx: &ServiceContext, query: SyncHistoryQuery) -> Result<Vec<HistoryRow>, ApiError> {
    let limit = if query.limit == 0 { DEFAULT_HISTORY_LIMIT } else { query.limit };
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
    let file_rows = list_outbound_files(&conn, id).map_err(|e| ApiError::Internal(format!("{e:#}")))?;
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
    let file_rows = list_inbound_files(&conn, id).map_err(|e| ApiError::Internal(format!("{e:#}")))?;
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
        .map(|e| TransferEventEntry { ts: e.ts, kind: e.kind, detail: e.detail })
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
pub async fn resolve_dest_node(ctx: &ServiceContext, device_id: &str) -> Result<ResolvedDest, ApiError> {
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
        ApiError::Invalid(format!("destination device {device_id} has an invalid pubkey: {e:#}"))
    })?;
    Ok(ResolvedDest { node, endpoint_addr: device.endpoint_addr.clone() })
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
    install_node_wake_hook(&node, Arc::clone(sender), Arc::clone(&collab_sender));
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
    if !prefix.components().any(|c| matches!(c, std::path::Component::Normal(_))) {
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
    batch_name: Option<&str>,
    frame_set_id: Option<i64>,
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

    let mut resolved: HashSet<i64> = HashSet::new();
    let mut ineligible: Vec<IneligibleFrame> = Vec::new();
    let mut eligible: Vec<i64> = Vec::new();
    let mut records: Vec<(PathBuf, ManifestRecord)> = Vec::new();
    // Per-directory used-filename sets for collision suffixing (§D2).
    let mut used_by_dir: HashMap<String, HashSet<String>> = HashMap::new();
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
        let rel_path = assign_rel_path(&layout, frame_id, file, &mut used_by_dir);

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
                project: None,
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
        return Ok(BuiltSelection {
            pkg_dir: None,
            eligible,
            ineligible,
            total,
            display_name: None,
            files: Vec::new(),
        });
    }

    // Resolve the batch's human name (§D1) and snapshot the per-file rows for the
    // `sync_outbound_files` write, before `records` is moved into the writer.
    let display_name =
        resolve_batch_name(batch_name, conn, frame_set_id, ancestor.as_deref(), records.len());
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
        total,
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
        build_selection_package(&conn, origin_device, packages_dir, frame_ids, batch_name, frame_set_id)?
    };
    if let Some(dir) = &built.pkg_dir {
        // §D1/§D4: the batch name + per-file `pending` rows travel WITH the enqueue
        // now — the engine's `store.enqueue` writes them in the same transaction as
        // the row, so the first announce can never read a nameless / file-less row
        // (the T3 enqueue→announce race is gone; no post-hoc write, no self-heal).
        let files: Vec<crate::sharing::types::AnnounceFileEntry> = built
            .files
            .iter()
            .map(|(rel_path, byte_size, frame_uuid)| crate::sharing::types::AnnounceFileEntry {
                rel_path: rel_path.clone(),
                byte_size: *byte_size,
                frame_uuid: frame_uuid.clone(),
            })
            .collect();
        engine
            .enqueue_package(dir, built.display_name.clone(), files)
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
pub async fn send_now_sync_package(sender: &Arc<SyncSenderRuntime>, id: i64) -> Result<(), ApiError> {
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
///   very first announce runs the epilogue.
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
        return Err(ApiError::Invalid("too late: ingest in progress".to_string()));
    }
    // Already terminal — no-op.
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
        Some(InboundState::Fetching) => control.is_none(),
        _ => false,
    };
    if stamp_now {
        let conn = store.lock_conn();
        set_inbound_state(&conn, package_id, InboundState::Cancelled, None)
            .map_err(|e| ApiError::Internal(format!("stamp inbound cancelled {package_id}: {e:#}")))?;
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
        delete_outbound_files, delete_outbound_row, delete_sync_events,
        inbound_id_states_by_batch, outbound_peer_hex, outbound_ref_states,
        settle_unsettled_outbound_files,
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
            let matching: Vec<(i64, OutboundState)> =
                matching_full.iter().map(|(id, _, state)| (*id, *state)).collect();
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
                if let Some((_, state, _)) =
                    non_terminal.iter().find(|(_, _, peer)| authorized.contains(peer))
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
            (DeletedTransferRecord { rows, history }, Reclaim::Payloads(payload_dirs))
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
            let mut history = delete_history_for_package(&tx, Direction::Received, &package_key)?;
            let wire_ids: Vec<String> =
                matched.iter().map(|(_, _, wire)| wire.clone()).collect();
            for wire in &wire_ids {
                if *wire != package_key {
                    history += delete_history_for_package(&tx, Direction::Received, wire)?;
                }
            }
            // Release the matched rows' CURRENT wire ids' blob tags (the batch key is
            // the stable `batch_uuid` now, which is NOT a tag — the wire id is).
            (DeletedTransferRecord { rows, history }, Reclaim::Tags(wire_ids))
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
        use crate::sync::store::{append_sync_event, insert_history_row, insert_outbound_with_files};

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
            a1 = insert_outbound_with_files(&conn, "/pkgs/batchA", &peer, Some("Batch A"), &files("f1")).unwrap();
            a2 = insert_outbound_with_files(&conn, "/pkgs/batchA", &peer, Some("Batch A"), &files("f2")).unwrap();
            let b1 = insert_outbound_with_files(&conn, "/pkgs/batchB", &peer, Some("Batch B"), &files("g1")).unwrap();
            for id in [a1, a2, b1] {
                conn.execute("UPDATE sync_outbound SET state = 'cancelled' WHERE id = ?1", [id]).unwrap();
                append_sync_event(&conn, Direction::Sent, &id.to_string(), "cancel", None).unwrap();
            }
            insert_history_row(&conn, &hist("f1", "batchA")).unwrap();
            insert_history_row(&conn, &hist("f2", "batchA")).unwrap();
            insert_history_row(&conn, &hist("g1", "batchB")).unwrap();
        }

        let deleted =
            delete_transfer_history(&ctx, &SyncRuntime::new(), Direction::Sent, "batchA".to_string())
                .await
                .expect("delete ok");
        assert_eq!(deleted.rows, 2, "both batchA attempt rows deleted");
        assert_eq!(deleted.history, 2, "both batchA history rows deleted");

        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        // batchA fully gone across all four record tables.
        assert_eq!(count("SELECT COUNT(*) FROM sync_outbound WHERE package_ref = '/pkgs/batchA'"), 0);
        assert_eq!(count(&format!("SELECT COUNT(*) FROM sync_outbound_files WHERE outbound_id IN ({a1},{a2})")), 0);
        assert_eq!(count(&format!("SELECT COUNT(*) FROM sync_events WHERE batch_key IN ('{a1}','{a2}')")), 0);
        assert_eq!(count("SELECT COUNT(*) FROM sync_history WHERE package_id = 'batchA'"), 0);
        // batchB intact — exactly its one file row, event, history row, and outbound row.
        assert_eq!(count("SELECT COUNT(*) FROM sync_outbound WHERE package_ref = '/pkgs/batchB'"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM sync_outbound_files"), 1, "only batchB's file row remains");
        assert_eq!(count("SELECT COUNT(*) FROM sync_events"), 1, "only batchB's event remains");
        assert_eq!(count("SELECT COUNT(*) FROM sync_history WHERE package_id = 'batchB'"), 1);
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
            let done = insert_outbound_with_files(&conn, "/pkgs/live", &peer, None, &files).unwrap();
            conn.execute("UPDATE sync_outbound SET state = 'cancelled' WHERE id = ?1", [done]).unwrap();
            let live = insert_outbound_with_files(&conn, "/pkgs/live", &peer, None, &files).unwrap();
            conn.execute("UPDATE sync_outbound SET state = 'transferring' WHERE id = ?1", [live]).unwrap();
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

        let err = delete_transfer_history(&ctx, &SyncRuntime::new(), Direction::Sent, "live".to_string())
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "active attempt refuses with Invalid");

        // Nothing deleted — both rows and the history row survive.
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_outbound WHERE package_ref = '/pkgs/live'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "both attempt rows still present");
        let h: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_history WHERE package_id = 'live'", [], |r| r.get(0))
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
            append_sync_event(&conn, Direction::Received, &id.to_string(), "fetch_done", None).unwrap();
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

        let deleted =
            delete_transfer_history(&ctx, &SyncRuntime::new(), Direction::Received, pkg.to_string())
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
        assert_eq!(count("SELECT COUNT(*) FROM sync_history WHERE direction = 'received'"), 0);
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
            assert_ne!(wire, BATCH, "wire id must differ from the batch_uuid for this test");
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
            append_sync_event(&conn, Direction::Received, &id.to_string(), "fetch_done", None).unwrap();
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

        let deleted =
            delete_transfer_history(&ctx, &SyncRuntime::new(), Direction::Received, BATCH.to_string())
                .await
                .expect("delete ok");
        assert_eq!(deleted.rows, 1, "the one batch row deleted (matched by batch_uuid)");
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
        assert_eq!(count("SELECT COUNT(*) FROM sync_history WHERE direction = 'received'"), 0);
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
            set_inbound_state(&conn, "wire-rev", InboundState::Cancelled, Some("by sender")).unwrap();
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
        use crate::sync::store::{append_sync_event, insert_history_row, insert_outbound_with_files};

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
            term = insert_outbound_with_files(&conn, "/pkgs/gone", &dead_peer, None, &files("f1")).unwrap();
            orphan = insert_outbound_with_files(&conn, "/pkgs/gone", &dead_peer, None, &files("f2")).unwrap();
            conn.execute("UPDATE sync_outbound SET state = 'cancelled' WHERE id = ?1", [term]).unwrap();
            conn.execute("UPDATE sync_outbound SET state = 'transferring' WHERE id = ?1", [orphan]).unwrap();
            append_sync_event(&conn, Direction::Sent, &term.to_string(), "cancel", None).unwrap();
            append_sync_event(&conn, Direction::Sent, &orphan.to_string(), "attempt", None).unwrap();
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

        let deleted = delete_transfer_history(&ctx, &SyncRuntime::new(), Direction::Sent, "gone".to_string())
            .await
            .expect("dead-peer orphan deletes");
        assert_eq!(deleted.rows, 2, "both attempt rows deleted");
        assert_eq!(deleted.history, 1, "the history row deleted");

        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM sync_outbound WHERE package_ref = '/pkgs/gone'"), 0);
        assert_eq!(count(&format!("SELECT COUNT(*) FROM sync_outbound_files WHERE outbound_id IN ({term},{orphan})")), 0);
        assert_eq!(count(&format!("SELECT COUNT(*) FROM sync_events WHERE batch_key IN ('{term}','{orphan}')")), 0);
        assert_eq!(count("SELECT COUNT(*) FROM sync_history WHERE package_id = 'gone'"), 0);
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
            let files = vec![AnnounceFileEntry { rel_path: "Lights/x.fits".into(), byte_size: 4, frame_uuid: "x".into() }];
            live = insert_outbound_with_files(&conn, "/pkgs/live", &live_peer, None, &files).unwrap();
            conn.execute("UPDATE sync_outbound SET state = 'transferring' WHERE id = ?1", [live]).unwrap();
        }

        let err = delete_transfer_history(&ctx, &SyncRuntime::new(), Direction::Sent, "live".to_string())
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
            .query_row("SELECT COUNT(*) FROM sync_outbound WHERE package_ref = '/pkgs/live'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "nothing deleted while the peer is still an account device");
        let state: String = conn
            .query_row("SELECT state FROM sync_outbound WHERE id = ?1", [live], |r| r.get(0))
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
            let files = vec![AnnounceFileEntry { rel_path: "Lights/x.fits".into(), byte_size: 4, frame_uuid: "x".into() }];
            let id = insert_outbound_with_files(&conn, "/pkgs/cold", &peer, None, &files).unwrap();
            conn.execute("UPDATE sync_outbound SET state = 'transferring' WHERE id = ?1", [id]).unwrap();
        }

        let err = delete_transfer_history(&ctx, &SyncRuntime::new(), Direction::Sent, "cold".to_string())
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
            .query_row("SELECT COUNT(*) FROM sync_outbound WHERE package_ref = '/pkgs/cold'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "cold cache never deletes — no guessing about liveness");
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
            )
            .unwrap();
            conn.execute("UPDATE sync_outbound SET state = 'confirmed' WHERE id = ?1", [id]).unwrap();
        }

        let deleted =
            delete_transfer_history(&ctx, &SyncRuntime::new(), Direction::Sent, "batchZ".to_string())
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
        use crate::sync::receiver::{allow_all_peers, IncomingResolver, InboundControl, SyncReceiver};
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
            crate::sync::store::set_inbound_state(&conn, wire, InboundState::Failed, Some("boom")).unwrap();
            let _ = id;
        }

        // A started receive-side runtime over a loopback endpoint that "serves"
        // the wire id (the loopback stand-in for the receiver's in-flight tag).
        let net = LoopbackNetwork::new();
        let ep = Arc::new(net.endpoint());
        let recv_store = Arc::new(CatalogSyncStore::open(tmp.path().join("recv.db")).unwrap());
        let staging = tmp.path().join("recv-staging");
        let incoming: IncomingResolver = { let p = tmp.path().join("recv-incoming"); Arc::new(move || p.clone()) };
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
            .set_started_for_test(Arc::clone(&ep) as Arc<dyn SharingTransport>, handle, "ticket".into())
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
        assert!(!ep.is_serving(wire), "delete releases the received batch's blob tags");
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
            let files = vec![AnnounceFileEntry { rel_path: "L/a.fits".into(), byte_size: 4, frame_uuid: "a".into() }];
            let sid = insert_outbound_with_files(&conn, "/pkgs/batch-out", &peer, Some("Out"), &files).unwrap();
            conn.execute("UPDATE sync_outbound SET state = 'confirmed' WHERE id = ?1", [sid]).unwrap();
            // A terminal received row with a NULL batch_uuid (upsert_inbound_announced
            // leaves it NULL) → batch_uuid falls back to the wire id.
            let rid = upsert_inbound_announced(&conn, &peer, "wire-in", 1, 10).unwrap();
            crate::sync::store::set_inbound_state(&conn, "wire-in", InboundState::Done, None).unwrap();
            let _ = rid;
        }

        let terminal = list_terminal_transfers(&ctx, None).expect("terminal transfers");
        let sent = terminal.sent.iter().find(|s| s.batch_uuid == "batch-out").expect("sent summary present");
        assert_eq!(sent.generation, 1, "a fresh sent row is at generation 1");
        assert_eq!(sent.batch_uuid, "batch-out", "sent batch_uuid == package-dir basename");

        let recv = terminal.received.iter().find(|r| r.package_id == "wire-in").expect("received summary present");
        assert_eq!(recv.generation, 1, "a fresh received row is at generation 1");
        assert_eq!(recv.batch_uuid, "wire-in", "a NULL-batch_uuid received row falls back to the wire id");
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
        let node3 = ensure_iroh_node(&ctx).await.expect("re-bind after shutdown");
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
        store.enqueue("/pkgs/a1", peer_a, None, &[]).unwrap();
        let a2 = store.enqueue("/pkgs/a2", peer_a, None, &[]).unwrap();
        store.set_state(a2, OutboundState::Transferring).unwrap();
        // peer_b: one non-terminal row + one terminal (confirmed).
        store.enqueue("/pkgs/b1", peer_b, None, &[]).unwrap();
        let b2 = store.enqueue("/pkgs/b2", peer_b, None, &[]).unwrap();
        store.set_state(b2, OutboundState::Confirmed).unwrap();
        // peer_c: every row terminal (failed) → must be excluded entirely.
        let c1 = store.enqueue("/pkgs/c1", peer_c, None, &[]).unwrap();
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
        assert_eq!(kept, vec![member], "only the account member survives the filter");
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
            store.enqueue("/pkgs/member", member, None, &[]).unwrap();
            store.enqueue("/pkgs/stranger", stranger, None, &[]).unwrap();
        }

        let authorized: HashSet<String> = [node_id_hex(&member)].into_iter().collect();
        let sender = Arc::new(SyncSenderRuntime::new());
        let called: Arc<std::sync::Mutex<Vec<NodeId>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let called_in = Arc::clone(&called);
        let ensured = resurrect_pending_senders_with(&db_path, &sender, Some(&authorized), move |peer| {
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
            let id1 = store.enqueue("/pkgs/one", peer_x, None, &[]).unwrap();
            let id2 = store.enqueue("/pkgs/two", peer_x, None, &[]).unwrap();
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
            StartedSender { engine, origin_device: node_id_hex(&peer), peer }
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
        assert_eq!(status.active.len(), 2, "2 distinct rows, not 4 (deduped across the 2 peers)");
        assert_eq!(
            status.active.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![1, 2],
            "deduped rows stay ordered ascending by id"
        );
        assert_eq!(status.queued, 1, "one Queued row, counted once");
        assert_eq!(status.transferring, 1, "one Transferring row, counted once");
        assert_eq!(status.queued + status.transferring, 2);
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
            endpoint_addr: None,
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

        // The device-names cache maps each decodable pubkey's hex → its current
        // name — the source the receiver reads to name incoming landing folders.
        let names = account_device_names(&devices);
        assert_eq!(names.len(), 3);
        assert_eq!(names.get(&"01".repeat(32)).map(String::as_str), Some("n1"));
        assert_eq!(names.get(&"02".repeat(32)).map(String::as_str), Some("n2"));
        assert_eq!(names.get(&"03".repeat(32)).map(String::as_str), Some("n3"));
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
        assert!(cached_device_names(&conn).is_empty(), "no cache yet → empty");

        // Present → hex→name lookups; an uncached hex is absent.
        let json = format!(r#"{{"{peer_a}":"My Mac"}}"#);
        crate::db::set_setting(&conn, keys::SYNC_DEVICE_NAMES, &json).unwrap();
        let map = cached_device_names(&conn);
        assert_eq!(map.get(&peer_a).map(String::as_str), Some("My Mac"));
        assert!(map.get(&peer_z).is_none(), "an uncached peer resolves to None");

        // Malformed JSON → empty map, never a panic/error.
        crate::db::set_setting(&conn, keys::SYNC_DEVICE_NAMES, "not json").unwrap();
        assert!(cached_device_names(&conn).is_empty(), "malformed cache → empty, best-effort");
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
            let id =
                crate::sync::store::upsert_inbound_announced(&conn, &peer, "pkg-v2", 2, 300).unwrap();
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
        assert_eq!(files.len(), 2, "both announced rows surface before any fetch");
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
                crate::sync::store::upsert_inbound_announced(&conn, &peer, "pkg-legacy", 1, 50).unwrap();
            crate::sync::store::set_inbound_state(&conn, "pkg-legacy", InboundState::Done, None).unwrap();
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
        assert_eq!(legacy[0].rel_path, "old.fits", "no rel_path in history → basename");
        assert_eq!(legacy[0].outcome.as_deref(), Some("ingested"));
        assert!(legacy[0].state.is_none(), "legacy fallback has no per-file state");
    }

    /// tv2 §D7: `list_transfer_events` returns a batch's journal newest-first, in
    /// the `{ ts, kind, detail }` shape, scoped to the (direction, row id) key.
    #[test]
    fn list_transfer_events_returns_newest_first() {
        let (_tmp, ctx) = test_ctx();
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            for (kind, detail) in
                [("enqueued", Some("frames=2")), ("announce_sent", None), ("uploaded", Some("bytes=300"))]
            {
                crate::sync::store::append_sync_event(&conn, Direction::Sent, "7", kind, detail).unwrap();
            }
            // A different batch's journal must not leak into batch 7.
            crate::sync::store::append_sync_event(&conn, Direction::Sent, "8", "enqueued", None).unwrap();
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
            build_selection_package(&conn, "origin-dev", &pkg_root, &[f1, f2], None, None).unwrap()
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
        let built = build_selection_package(&conn, "origin-dev", &pkg_root, &[f1, f2], None, None).unwrap();
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
            build_selection_package(&conn, "origin-dev", &pkg_root, &[f1, f2, unknown], None, None)
                .unwrap()
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
        // `enqueue_sync_selection` now takes an owned `Arc<ServiceContext>` (it
        // builds a `'static` retry address-refresher from it, T8).
        let ctx = Arc::new(ctx);
        let sender = Arc::new(SyncSenderRuntime::new());
        let collab_sender = Arc::new(SyncSenderRuntime::new());
        let sync = SyncRuntime::new();
        let dest = ResolvedDest { node: [7u8; 32], endpoint_addr: None };

        let result =
            enqueue_sync_selection(&ctx, &sender, collab_sender, &sync, dest, Vec::new(), None, None, None)
                .await
                .unwrap();
        assert_eq!(result.enqueued_count, 0);
        assert_eq!(result.eligible_count, 0);
        assert_eq!(result.total_count, 0);
        assert!(result.ineligible.is_empty());
        assert!(!sender.is_started().await, "no engine started for an empty selection");
        assert!(sender.started_peers().await.is_empty(), "no peer engine for an empty selection");
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
        build_selection_package(&conn, "origin-dev", &pkg_root, &[f1], None, None)
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
            SyncConfig { ack_timeout: Duration::from_secs(60) },
        ));
        let sender = Arc::new(SyncSenderRuntime::new());
        sender
            .lock_inner()
            .await
            .insert(peer, StartedSender { engine, origin_device: node_id_hex(&peer), peer });
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
        assert!(!manifest.is_empty(), "fixture package has >=1 manifest record");
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
            .enqueue_package(&pkg_dir, Some(display.clone()), files.clone())
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
        let same_id =
            resend_transfer(&ctx, &sender, Arc::clone(&collab_sender), &sync, id, None)
                .await
                .unwrap();
        assert_eq!(same_id, id, "resend returns the SAME transfer id (no new row)");

        // The reset ran synchronously inside `resend_transfer` before the async
        // re-drive; read it straight back.
        let after = {
            let store = CatalogSyncStore::open(&db_path).unwrap();
            store.get_outbound(id).unwrap().expect("row still exists")
        };
        assert!(!after.state.is_terminal(), "resend un-terminalizes the SAME row");
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
        assert_eq!(file_rows.len(), manifest.len(), "every manifest file has a row");
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
        assert!(cancelled_at.is_some(), "journal has a cancelled event, got {kinds:?}");
        assert!(resend_at.is_some(), "journal has a resend event, got {kinds:?}");
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
        let id = engine.enqueue_package(&pkg_dir, None, Vec::new()).await.unwrap();

        let err = resend_transfer(&ctx, &sender, collab_sender, &sync, id, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ApiError::Invalid(_)),
            "resend rejects a pending (non-terminal) package, got {err:?}",
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
            build_selection_package(&conn, "origin-dev", &pkg_root, &[f1, f2], None, None)
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
                    if receiver.fetch(from, &announce, &dest, noop_fetch_sink()).await.is_ok() {
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
        let id = engine.enqueue_package(&pkg_dir, None, Vec::new()).await.unwrap();

        // Poll until confirmed (bounded).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let confirmed = store.get_outbound(id).unwrap().map(|r| r.state)
                == Some(OutboundState::Confirmed);
            if confirmed {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "package never confirmed");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let files = list_transfer_files(&ctx, Direction::Sent, id).unwrap();
        assert_eq!(files.len(), 2, "one entry per manifest frame");
        assert!(
            files.iter().all(|f| f.outcome.as_deref() == Some("ingested")),
            "confirmed frames report the peer's ingested verdict: {files:?}"
        );
        assert!(
            files.iter().any(|f| f.name == "light-0001.fits" && f.bytes_total > 0),
            "the manifest supplies name + bytes: {files:?}"
        );
        assert!(files.iter().all(|f| f.bytes_done.is_none()), "sent entries carry no bytes_done");

        // tv2 follow-up: the confirmed row (a REAL engine terminal row, not a
        // seeded one) surfaces in `list_terminal_transfers().sent` so it survives
        // a restart — where the in-memory page ledger would not.
        let terminal = list_terminal_transfers(&ctx, None).unwrap();
        let confirmed = terminal.sent.iter().find(|s| s.id == id).expect("confirmed row in terminal sent");
        assert_eq!(confirmed.display_state, "confirmed");
        assert_eq!(confirmed.file_count, 2, "the confirmed batch's manifest enriches its terminal summary");

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
            build_selection_package(&conn, "origin-dev", &pkg_root2, &[f1, f2], None, None)
                .unwrap()
                .pkg_dir
                .expect("a package was written")
        };
        let swept_dir = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            build_selection_package(&conn, "origin-dev", &pkg_root2, &[f1, f2], None, None)
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
            )
            .unwrap();
            let swept = crate::sync::store::insert_outbound_with_files(
                &conn,
                &swept_dir.to_string_lossy(),
                &peer_hex,
                Some("Cancelled — payload swept"),
                &[],
            )
            .unwrap();
            (intact, swept)
        };
        set_outbound_terminal(&ctx, intact_id, "cancelled", "2026-07-20T09:00:00.000Z");
        set_outbound_terminal(&ctx, swept_id, "cancelled", "2026-07-20T09:30:00.000Z");

        let terminal2 = list_terminal_transfers(&ctx, None).unwrap();
        let intact = terminal2.sent.iter().find(|s| s.id == intact_id).expect("intact cancelled row");
        assert!(intact.resendable, "a cancelled row with its payload intact on disk is resendable");
        let swept = terminal2.sent.iter().find(|s| s.id == swept_id).expect("swept cancelled row");
        assert!(!swept.resendable, "a cancelled row whose payload was cleaned up is NOT resendable");

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
            let pkg_dir = build_selection_package(&conn, "origin-dev", &pkg_root, &[f1], None, None)
                .unwrap()
                .pkg_dir
                .expect("a package was written");
            drop(conn);
            let store = CatalogSyncStore::open(&db_path).unwrap();
            let id = store.enqueue(&pkg_dir.to_string_lossy(), [9u8; 32], None, &[]).unwrap();
            (pkg_dir, id)
        };
        let _ = pkg_dir;

        let files = list_transfer_files(&ctx, Direction::Sent, id).unwrap();
        assert_eq!(files.len(), 1, "manifest lists the one frame even with no ack yet");
        assert_eq!(files[0].name, "light-0001.fits");
        assert!(files[0].outcome.is_none(), "no verdict while the send is in flight");
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
        assert!(files.iter().all(|f| f.outcome.as_deref() == Some("ingested")));
        let a = files.iter().find(|f| f.name == "a.fits").expect("a.fits present");
        assert_eq!(a.bytes_total, 100);
        assert_eq!(a.bytes_done, Some(100), "terminal received: bytes_done reflects received bytes");
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
        use crate::sync::store::{insert_outbound_with_files, set_inbound_state, upsert_inbound_announced};
        use crate::sync::InboundState;

        let (_tmp, ctx) = test_ctx();
        let peer = "aa".repeat(32);

        // Seed outbound rows in a known order (ids ascending): a non-terminal
        // queued row that must be excluded, then confirmed → failed → cancelled.
        // The cancelled row carries a display_name + 2 per-file rows.
        let (queued_id, confirmed_id, failed_id, cancelled_id) = {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let queued = insert_outbound_with_files(&conn, "/pkg/queued", &peer, Some("Queued batch"), &[]).unwrap();
            let confirmed = insert_outbound_with_files(&conn, "/pkg/confirmed", &peer, Some("Confirmed batch"), &[]).unwrap();
            let failed = insert_outbound_with_files(&conn, "/pkg/failed", &peer, Some("Failed batch"), &[]).unwrap();
            let files = [
                AnnounceFileEntry { rel_path: "cam/a.fits".into(), byte_size: 100, frame_uuid: "u-a".into() },
                AnnounceFileEntry { rel_path: "cam/b.fits".into(), byte_size: 200, frame_uuid: "u-b".into() },
            ];
            let cancelled = insert_outbound_with_files(&conn, "/pkg/cancelled", &peer, Some("Nightly batch"), &files).unwrap();
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
        assert_eq!(sent_ids, vec![cancelled_id, failed_id, confirmed_id], "sent: terminal only, newest-first");
        assert!(!sent_ids.contains(&queued_id), "a non-terminal queued row must be excluded");

        let recv_states: Vec<&str> = terminal.received.iter().map(|r| r.display_state.as_str()).collect();
        assert_eq!(recv_states, vec!["cancelled", "failed", "done"], "received: terminal only, newest-first");
        assert!(
            !terminal.received.iter().any(|r| r.package_id == "in-announced"),
            "a non-terminal announced inbound row must be excluded"
        );

        // The cancelled outbound summary carries its display_name + file_counts.
        let cancelled = terminal.sent.iter().find(|s| s.id == cancelled_id).unwrap();
        assert_eq!(cancelled.display_name.as_deref(), Some("Nightly batch"));
        assert_eq!(cancelled.display_state, "cancelled");
        assert_eq!(cancelled.file_counts.total, 2, "per-file rows counted into the summary");

        // Limit is honored (newest row only).
        let one = list_terminal_transfers(&ctx, Some(1)).unwrap();
        assert_eq!(one.sent.len(), 1);
        assert_eq!(one.sent[0].id, cancelled_id, "limit keeps the newest terminal row");
        assert_eq!(one.received.len(), 1);

        // Restart-survival: the cancelled row's per-file rows are still listable
        // by its durable id, read straight from the DB (no in-memory ledger).
        let files = list_transfer_files(&ctx, Direction::Sent, cancelled_id).unwrap();
        assert_eq!(files.len(), 2, "the cancelled batch's per-file rows survive as detail");
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
            metadata_hash: None,
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
        assert_eq!(common_ancestor(&under_one), Some(PathBuf::from("/data/M31")));

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
            assign_rel_path(&layout, 1, &file_at("/data/M31/lights/a.fits", "a.fits"), &mut used),
            "lights/a.fits"
        );
        assert_eq!(
            assign_rel_path(&layout, 2, &file_at("/data/M31/b.fits", "b.fits"), &mut used),
            "b.fits"
        );

        // Flat fallback (disjoint roots): a repeated basename gets `_2`.
        let flat = RelPathLayout::SourceRelative(None);
        let mut used2: HashMap<String, HashSet<String>> = HashMap::new();
        assert_eq!(
            assign_rel_path(&flat, 1, &file_at("/vol_a/x/dup.fits", "dup.fits"), &mut used2),
            "dup.fits"
        );
        assert_eq!(
            assign_rel_path(&flat, 2, &file_at("/vol_b/y/dup.fits", "dup.fits"), &mut used2),
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
        assert_eq!(resolve_batch_name(None, &conn, Some(7), None, 3), "Andromeda Core");
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
        assert_eq!(row.display_name.as_deref(), Some("M31_data"), "auto name = ancestor basename");

        let files = crate::sync::store::list_outbound_files(&conn, row.id).unwrap();
        assert_eq!(files.len(), 2, "one row per manifest record");
        let rel: HashSet<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        assert!(rel.contains("lights/light1.fits"), "subdir preserved: {rel:?}");
        assert!(rel.contains("flats/flat1.fits"), "subdir preserved: {rel:?}");
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
        std::fs::write(abs_path, format!("payload-{}", abs_path.display()).as_bytes()).unwrap();
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
        crate::db::insert_frame(&conn, &frame).unwrap()
    }
}
