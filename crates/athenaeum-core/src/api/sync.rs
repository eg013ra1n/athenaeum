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
use crate::sync::store::{
    get_inbound_by_row_id, inbound_active, outbound_row_by_id, search_history_rows,
};
use crate::sync::{
    node_id_hex, pairing, CatalogSyncStore, Direction, HistoryQuery, HistoryRow, InboundSummary,
    OutboundRow, OutboundState, OutboundSummary, RefusalRefresher, StartedSender, SyncEngine,
    SyncEngineHandle, SyncReceiverStatus, SyncRuntime, SyncSenderRuntime, SyncSenderStatus,
    SyncStatus, SyncStore, TransferFileEntry, TransportHealth,
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
    sync: &SyncRuntime,
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
    ensure_peers_refresh_task(&ctx_arc, sync).await;
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
    sync.ensure_started(node, sync_dir, db_path, incoming, authorized, hooks, emitter)
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
    ctx: Arc<ServiceContext>,
    sync: &SyncRuntime,
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
    ensure_peers_refresh_task(&ctx_arc, sync).await;
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

    let ticket = sync
        .ensure_started(node, sync_dir, db_path, incoming, authorized, hooks, emitter)
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
    let active: Vec<OutboundSummary> = dedup_active_rows(active_rows)
        .into_iter()
        .map(|row| {
            let (file_count, byte_size) = package_totals(Path::new(&row.package_ref));
            OutboundSummary {
                id: row.id,
                package_short: short_pkg(&row.package_ref),
                state: row.state,
                attempts: row.attempts,
                created_at: row.created_at,
                peer_short: short_id(&node_id_hex(&row.peer)),
                last_error: row.last_error,
                next_retry_at: row.next_retry_at,
                byte_size,
                file_count,
            }
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

    let (confirmed_total, failed_total, cancelled_total) = {
        let db = db(ctx)?;
        let conn = db.conn();
        (
            count_outbound_state(&conn, "confirmed")?,
            count_outbound_state(&conn, "failed")?,
            count_outbound_state(&conn, "cancelled")?,
        )
    };

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
    Ok(rows
        .into_iter()
        .map(|r| InboundSummary {
            id: r.id,
            package_id: r.package_id.clone(),
            package_short: short_id(&r.package_id),
            peer_short: short_id(&r.peer),
            state: r.state,
            frame_count: r.frame_count,
            byte_size: r.byte_size,
            bytes_done: r.bytes_done,
            created_at: r.created_at,
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

/// Per-file detail for one transfer batch (Task 14) — the Transfers UI's
/// expand-a-row view. `direction` selects the outbound (`Sent`) or inbound
/// (`Received`) half; `id` is the durable row id in that half
/// (`sync_outbound.id` / `sync_inbound.id`, from the corresponding summary).
///
/// **Sent:** the package's manifest (read from the outbound row's `package_ref`)
/// is the authoritative file list — name + bytes per frame. Per-frame outcomes
/// come from THIS sender's own confirmed/terminal history rows for the package,
/// joined by the stable package-dir-basename batch key: a frame shows
/// `ingested`/`duplicate`/`rejected`/… once the peer's ack lands, and `None`
/// while the send is still in flight. (Receipts live only on the *receiver*, so a
/// real two-machine sender's own history — not a `sync_receipts` read that would
/// be empty there — is the durable per-frame verdict.)
///
/// **Received:** when the inbound row is terminal, entries come from this node's
/// received-history rows for the package (`WHERE package_id = <wire id>`); while
/// the fetch is still active, the staged manifest backfills names/sizes if it has
/// landed, else an empty list (the live per-file bars are event-driven via
/// `sync-file-progress`).
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

    let dir = PathBuf::from(&row.package_ref);
    let records = package::read_manifest(&dir)
        .map_err(|e| ApiError::Internal(format!("read manifest {}: {e:#}", dir.display())))?;

    // This package's settled per-frame verdicts: the sender's confirmed/terminal
    // rows carry `finished_at`; the started (`sent`) rows do not. Newest-first, so
    // the first row seen per frame_uuid wins (a redelivery's latest verdict).
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
            name: detail_filename(&r.rel_path),
            bytes_total: r.byte_size,
            bytes_done: None,
            outcome: outcome_by_frame.get(&r.frame_uuid).cloned(),
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

    if row.state.is_terminal() {
        // History is the durable record of what landed. Newest-first; keep the
        // first row per frame_uuid (a redelivery's latest verdict).
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
                    name: h.filename,
                    bytes_total: h.bytes,
                    bytes_done: Some(h.bytes),
                    outcome: Some(h.outcome),
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
                name: detail_filename(&r.rel_path),
                bytes_total: r.byte_size,
                bytes_done: None,
                outcome: None,
            })
            .collect()),
        Err(_) => Ok(Vec::new()),
    }
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
    ctx: &Arc<ServiceContext>,
    sender: &Arc<SyncSenderRuntime>,
    collab_sender: Arc<SyncSenderRuntime>,
    sync: &SyncRuntime,
    dest: ResolvedDest,
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

/// Retry a terminal outbound package: re-enqueue a `Failed` / `Cancelled`
/// package's dir as a NEW durable row — the sanctioned retry model (the receiver
/// dedups by frame uuid; the original terminal row is left intact). Ported from
/// Perseus's `api_retry`: the row must be terminal, its payload must still be on
/// disk, then `enqueue_package` mints the new row id.
///
/// The re-enqueue MUST run on the engine whose peer matches `row.peer` — the
/// worker stamps the new row with the ENGINE's peer, so enqueueing on any other
/// peer's engine would send the package to the wrong device. A started engine for
/// `row.peer` is reused; otherwise one is built lazily via [`ensure_sender_engine`]
/// (a restart clears the in-memory engine map while the terminal rows persist, so
/// a retry after a restart legitimately has no engine yet). The first-attempt dial
/// hint falls back to our own relay set (`dest_endpoint_addr = None`); the engine's
/// T8 address refresher re-resolves the peer's real address on retry.
pub async fn retry_sync_package(
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
    // Only a terminal failed/cancelled row is retryable (a confirmed row is
    // manifest-only after cleanup; a live row is send-now territory, not retry).
    if !matches!(row.state, OutboundState::Failed | OutboundState::Cancelled) {
        return Err(ApiError::Invalid(format!(
            "package {id} is {} — only failed or cancelled packages can be retried",
            row.state.as_str()
        )));
    }
    // The payload must still be on disk — re-announcing a manifest-only dir can
    // never deliver.
    let dir = PathBuf::from(&row.package_ref);
    if !package_has_payload(&dir) {
        return Err(ApiError::Invalid(format!(
            "package {id} data is missing on disk; cannot retry"
        )));
    }
    // Enqueue on row.peer's engine — reuse a started one or build it lazily.
    let engine = match sender.current_for(&row.peer).await {
        Some((engine, _)) => engine,
        None => {
            let (engine, _origin) =
                ensure_sender_engine(ctx, sender, collab_sender, sync, row.peer, None, emitter)
                    .await?;
            engine
        }
    };
    let new_id = engine
        .enqueue_package(&dir)
        .await
        .map_err(|e| ApiError::Internal(format!("re-enqueue package {id}: {e:#}")))?;
    tracing::info!(old_id = id, new_id, peer = %node_id_hex(&row.peer), "sync package retried");
    Ok(new_id)
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
            let id1 = store.enqueue("/pkgs/one", peer_x).unwrap();
            let id2 = store.enqueue("/pkgs/two", peer_x).unwrap();
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
        // `enqueue_sync_selection` now takes an owned `Arc<ServiceContext>` (it
        // builds a `'static` retry address-refresher from it, T8).
        let ctx = Arc::new(ctx);
        let sender = Arc::new(SyncSenderRuntime::new());
        let collab_sender = Arc::new(SyncSenderRuntime::new());
        let sync = SyncRuntime::new();
        let dest = ResolvedDest { node: [7u8; 32], endpoint_addr: None };

        let result =
            enqueue_sync_selection(&ctx, &sender, collab_sender, &sync, dest, Vec::new(), None)
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
        build_selection_package(&conn, "origin-dev", &pkg_root, &[f1])
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

    /// Step 1: enqueue → cancel (terminal `Cancelled`) → `retry_sync_package`
    /// re-enqueues the SAME package dir as a NEW durable row (new id ≠ old); the
    /// original terminal row is left untouched and the new row is pending.
    #[tokio::test]
    async fn retry_reenqueues_terminal_package_as_new_row() {
        let (tmp, ctx) = test_ctx();
        let pkg_dir = build_one_frame_package(&ctx, tmp.path());
        let ctx = Arc::new(ctx);
        let db_path = db(&ctx).unwrap().path().to_path_buf();

        let (sender, peer) = loopback_sender_for(&db_path).await;
        let collab_sender = Arc::new(SyncSenderRuntime::new());
        let sync = SyncRuntime::new();

        // Enqueue on the peer's engine, then cancel it → terminal Cancelled.
        let (engine, _) = sender.current_for(&peer).await.unwrap();
        let old_id = engine.enqueue_package(&pkg_dir).await.unwrap();
        engine.cancel(old_id).await.unwrap();
        wait_outbound_state(&db_path, old_id, OutboundState::Cancelled).await;

        // Retry re-enqueues the SAME dir as a NEW row on the same peer's engine.
        let new_id =
            retry_sync_package(&ctx, &sender, Arc::clone(&collab_sender), &sync, old_id, None)
                .await
                .unwrap();
        assert_ne!(new_id, old_id, "retry mints a new durable row");

        let store = CatalogSyncStore::open(&db_path).unwrap();
        assert_eq!(
            store.get_outbound(old_id).unwrap().unwrap().state,
            OutboundState::Cancelled,
            "the original terminal row is left untouched",
        );
        let new_row = store.get_outbound(new_id).unwrap().expect("new row exists");
        assert!(!new_row.state.is_terminal(), "the re-enqueued row is pending");
        assert_eq!(
            new_row.package_ref,
            pkg_dir.to_string_lossy(),
            "the new row points at the same package dir",
        );
    }

    /// Step 1: `retry_sync_package` refuses a non-terminal (pending) package — only
    /// a `Failed` / `Cancelled` row is a retry candidate.
    #[tokio::test]
    async fn retry_rejects_pending_package() {
        let (tmp, ctx) = test_ctx();
        let pkg_dir = build_one_frame_package(&ctx, tmp.path());
        let ctx = Arc::new(ctx);
        let db_path = db(&ctx).unwrap().path().to_path_buf();

        let (sender, peer) = loopback_sender_for(&db_path).await;
        let collab_sender = Arc::new(SyncSenderRuntime::new());
        let sync = SyncRuntime::new();

        // Enqueue but do NOT cancel — the row is non-terminal (Queued/Announced).
        let (engine, _) = sender.current_for(&peer).await.unwrap();
        let id = engine.enqueue_package(&pkg_dir).await.unwrap();

        let err = retry_sync_package(&ctx, &sender, collab_sender, &sync, id, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ApiError::Invalid(_)),
            "retry rejects a pending (non-terminal) package, got {err:?}",
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
            build_selection_package(&conn, "origin-dev", &pkg_root, &[f1, f2])
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
        let id = engine.enqueue_package(&pkg_dir).await.unwrap();

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
            let pkg_dir = build_selection_package(&conn, "origin-dev", &pkg_root, &[f1])
                .unwrap()
                .pkg_dir
                .expect("a package was written");
            drop(conn);
            let store = CatalogSyncStore::open(&db_path).unwrap();
            let id = store.enqueue(&pkg_dir.to_string_lossy(), [9u8; 32]).unwrap();
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

        let started = autostart_if_enabled(
            Arc::new(ctx),
            &sync,
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
        let sync = SyncRuntime::new();
        let started = autostart_if_enabled(
            Arc::new(ctx),
            &sync,
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
}
