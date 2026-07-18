//! The single process-wide iroh node (iroh hardening C1/§1–§2).
//!
//! [`SharedIrohNode`] owns **one** iroh [`Endpoint`], **one** [`Router`] with
//! both ALPNs mounted once, and **one** [`FsStore`] — and hands out per-role
//! [`SharingTransport`] handles ([`Role::Recv`]/[`Role::Out`]/[`Role::Collab`]).
//! Before this, production bound up to three endpoints from the SAME device key
//! (personal sender + collab sender + receiver); a relay permits exactly one
//! active connection per node id, so they evicted each other and inbound
//! datagrams reached only whichever endpoint currently held the relay slot (the
//! 2026-07-12 field incident). Collapsing them onto one endpoint removes that
//! self-collision.
//!
//! The node is a refactor of [`IrohTransport`](super::IrohTransport) — it reuses
//! that module's construction shape, protocol handlers
//! ([`SyncControlProtocol`](super::SyncControlProtocol),
//! [`GatedBlobs`](super::GatedBlobs)) — mounted via the shared
//! [`build_router`](super::build_router) constructor — blob glue ([`blobs`]), and
//! connection-path diagnostics
//! verbatim, adding: a process-lifetime advisory **lock** on the device-key file
//! (I4), **role-prefixed** blob tags with a **prefix-scoped** startup sweep (Д3,
//! so one role's sweep can't wipe another's live tags on the shared store), a
//! **home-relay status** watcher, and an idempotent, bounded **shutdown** (I1).
//! `IrohTransport` stays alive as the loopback/test engine until Task 3 migrates
//! every production call site onto this node.
//!
//! Event demux and the pooled control connection are **Task 2** and now live
//! here: a per-`(peer, package)` ack-claim + single Recv-consumer [`EventDemux`]
//! (Д4) routes each inbound event to exactly one consumer (never the ambiguous
//! shared stream that let a misrouted ack be silently dropped, audit C1), and a
//! per-peer [`ControlPool`] reuses one idle-closed QUIC connection per peer
//! instead of dialing per control message.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use iroh::endpoint::{presets, Connection};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, Watcher};
use iroh_blobs::api::Store;
use iroh_blobs::store::fs::options::Options as FsOptions;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::store::GcConfig;
use iroh_blobs::Hash;
use iroh_tickets::endpoint::EndpointTicket;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

use crate::account::keys::{device_key_path, DeviceKey, DeviceKeyLock};
use crate::sharing::types::{
    FrameReceipt, NodeId, PackageAnnounce, PackageId, StartInfo, TransportEvent,
};
use crate::sharing::{FetchSink, SharingTransport};
use crate::sync::status::TransportHealth;
use crate::sync::DedupResponder;

use super::proto::{self, Msg, OfferEntry};
use super::{
    blobs, build_router, hex32, spawn_conn_path_diagnostics, ConnectGate, Delivery, EventSink,
    ServeRootResolver, SharedConnectGate, SharedResponder, CONTROL_SEND_TIMEOUT,
    EVENT_CHANNEL_CAPACITY, GC_INTERVAL, MAX_CONTROL_BYTES, ONLINE_TIMEOUT, SYNC_ALPN,
};

/// Upper bound on the graceful `endpoint.close()` at shutdown (I1). A clean
/// close lets peers see a QUIC close instead of a reset and clears the relay
/// registration promptly; if it stalls we log and move on rather than hang exit.
const SHUTDOWN_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Idle window before a pooled control connection is closed and evicted (Task 2).
/// Long enough that a burst of announces/acks reuses one connection; short enough
/// that a peer gone quiet doesn't hold an endpoint slot indefinitely.
const CONTROL_POOL_IDLE: Duration = Duration::from_secs(60);

/// After a probe's control connection establishes, how long to watch for a
/// peer-initiated close before calling the holder reachable (Task 9). A refusing
/// [`ConnectGate`] closes the fresh connection within milliseconds
/// (`close(0, "unauthorized")`), so a short window cleanly separates a
/// *refused* holder (closed) from a *reachable* one (stays open); a reachable
/// holder always waits the full window.
const PROBE_CLOSE_WINDOW: Duration = Duration::from_secs(1);

/// Why a short holder-reachability probe failed (Task 9), recorded per holder in
/// the download orchestrator's transfer-history detail. Best-effort classification
/// of a control-connect probe outcome: the real network can only ever be
/// approximated here, so a class is a diagnostic hint, never an authorization
/// signal (S5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeClass {
    /// No route / no addressing info (or a dial timeout with no relay hint) — the
    /// holder looks offline.
    Offline,
    /// The control connection established but the peer refused it (its
    /// [`ConnectGate`] closed the connection) — the holder is up but declined us.
    Refused,
    /// A dial timeout with a relay hint present — the relay path is unreachable.
    RelayUnreachable,
}

impl ProbeClass {
    /// Stable lowercase tag for the transfer-history detail / log field.
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeClass::Offline => "offline",
            ProbeClass::Refused => "refused",
            ProbeClass::RelayUnreachable => "relay_unreachable",
        }
    }
}

/// Classify a *failed* control-dial (the `connect()` returned an error) into a
/// [`ProbeClass`] (Task 9). Pure over the error text + whether a relay hint was
/// present, so it is unit-testable without a network. A peer that refused the
/// connection surfaces as an application close ("unauthorized"/"closed by peer");
/// a relay-only route that timed out is `RelayUnreachable`; anything else (no
/// addressing information, no route) is `Offline`.
fn classify_connect_err(msg: &str, has_relay_hint: bool) -> ProbeClass {
    let m = msg.to_lowercase();
    if m.contains("unauthorized")
        || m.contains("closed by peer")
        || m.contains("application")
        || m.contains("refused")
        || m.contains("forbidden")
    {
        ProbeClass::Refused
    } else if has_relay_hint && (m.contains("timed out") || m.contains("timeout")) {
        ProbeClass::RelayUnreachable
    } else {
        ProbeClass::Offline
    }
}

/// How often the relay-map refresh loop re-runs its resolver (H2, Task 8). Hourly
/// is a slack cadence — a relay-map change is rare and the sign-in path triggers
/// an immediate refresh out of band ([`SharedIrohNode::request_relay_refresh`]).
const RELAY_REFRESH_INTERVAL: Duration = Duration::from_secs(3600);

/// Poll cadence used *only while a rebuild is pending but the node is not yet
/// idle* (H2): the loop re-checks the idle gate this often so a deferred rebuild
/// fires within seconds of the node going quiet, without re-running the resolver.
const RELAY_REBUILD_RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// Upper bound on how long a pending rebuild may be deferred waiting for idle
/// before it is forced through regardless (H2). Bounding relay-map staleness is
/// the whole point of the task, so an indefinitely-busy node must not pin a stale
/// relay map forever; past this the rebuild runs at the next loop tick even if a
/// serve/fetch is in flight (a `warn!` records the forced disruption).
const RELAY_REBUILD_MAX_DEFER: Duration = Duration::from_secs(6 * 3600);

/// A `Send` boxed future — the return type of the injected relay resolver and (in
/// the engine) the retry address refresher. Defined locally so no `futures`/
/// `n0-future` boxing type leaks into the node's public signature.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// The host-injected, hub-agnostic relay-map resolver the refresh loop re-runs
/// (H2, Task 8). Returns the CURRENT `(RelayMode, relay_urls)` the transport
/// should be on — the app re-runs `resolve_relay_mode`, Perseus re-runs its relay
/// resolution — or `None` when it can't resolve right now (hub blip), in which
/// case the loop keeps the existing relay map. The node stays hub-agnostic: it
/// never reaches the hub itself, it only calls this callback.
pub type RelayResolver =
    Arc<dyn Fn() -> BoxFuture<Option<(RelayMode, Vec<String>)>> + Send + Sync>;

/// A transport-level wake hook (Task 6, sync delivery-forever). Invoked when the
/// node comes back online — a home-relay **reconnect** transition in the watcher,
/// or an **applied relay-map change** in the refresh loop — so the host can kick
/// every pending outbound package out of its backoff at once (the api layer's
/// closure fans a `SyncSenderRuntime::kick_all` over the personal + collab sender
/// maps). Installed via [`SharedIrohNode::set_wake_hook`]; `None` until then, so
/// every fire before install is a silent no-op. Stored behind an `Arc<RwLock<…>>`
/// so the home-relay watcher task — respawned on every relay rebuild — reads the
/// CURRENT hook at fire time rather than capturing a stale clone at spawn.
pub type WakeHook = Arc<dyn Fn() + Send + Sync>;

/// The node's last-known home-relay connectivity (Task 3.3). Written by the
/// home-relay watcher on every transition (the same values it already computes
/// for its log lines) and read by [`SharedIrohNode::transport_health`] on the
/// status poll. Held behind an `Arc<RwLock<…>>` on the rebuild-surviving OUTER
/// layer of the node (like [`WakeHook`]) — the watcher is respawned per relay
/// rebuild but writes into the SAME cell, so a poll always sees the current
/// picture. The initial value is disconnected; the successful one-shot
/// `online()` wait seeds it connected as a bridge, and the watcher overwrites it
/// on every later transition — so it is the sole connected-ness authority and a
/// dropped relay always flips the surface back to `direct_only`.
#[derive(Debug, Clone)]
pub struct RelayHealth {
    /// Whether a home relay is currently connected (last transition seen).
    pub connected: bool,
    /// The relay URL of the last transition (connect or disconnect), if any.
    pub url: Option<String>,
    /// The relay error from the last disconnect transition, if any.
    pub last_error: Option<String>,
    /// When this snapshot was recorded.
    pub since: Instant,
}

impl RelayHealth {
    /// The pre-transition disconnected baseline the node binds with.
    fn initial() -> Self {
        Self { connected: false, url: None, last_error: None, since: Instant::now() }
    }
}

/// Which multiplexed role a [`SharedIrohNode`] handle plays. The variant selects
/// the blob-tag prefix (Д3) — `recv/pkg/…`, `out/pkg/…`, `collab/pkg/…` — so the
/// three roles coexist on the node's single [`FsStore`] without clobbering each
/// other's tags, and each role's startup sweep is scoped to its own prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The personal-sync / collab receiver (inbound announces, blob pulls).
    Recv,
    /// The personal-sync sender (outbound package announces + serves).
    Out,
    /// The collaboration exchange sender (project announces + serves).
    Collab,
}

impl Role {
    /// The blob-tag prefix + startup-sweep scope for this role (Д3).
    pub(crate) fn prefix(self) -> &'static str {
        match self {
            Role::Recv => "recv",
            Role::Out => "out",
            Role::Collab => "collab",
        }
    }
}

/// The role-scoped, deterministic blob-store tag for a package collection —
/// `<prefix>/pkg/<package_id>`. The node's single namer (Д3), replacing the
/// role-agnostic [`package_tag`](super::package_tag): every role prefixes its
/// tags so a sibling role's startup sweep can never delete them.
fn role_package_tag(prefix: &str, package_id: &PackageId) -> String {
    format!("{prefix}/pkg/{}", package_id.0)
}

/// Resolve a served collection's root hash back to the [`PackageId`] whose
/// [`serve`](SharingTransport::serve) registered it (Task 13), by scanning the
/// role-prefixed `served` map (`<prefix>/pkg/<package_id>` → hash). The package id
/// is the segment after `/pkg/`. `None` for a child blob / hash-seq-internal /
/// foreign hash — the provider-events consumer still drains those, it just emits
/// no progress for them. Backs both [`SharedIrohNode::resolve_served_root`] and the
/// [`ServeRootResolver`] closure the consumer holds.
fn resolve_served_root_in(
    served: &Mutex<HashMap<String, Hash>>,
    hash: Hash,
) -> Option<PackageId> {
    let map = served.lock().expect("served mutex poisoned");
    map.iter().find_map(|(tag, h)| {
        if *h == hash {
            tag.split("/pkg/").nth(1).map(|id| PackageId(id.to_string()))
        } else {
            None
        }
    })
}

/// Per-`(peer, package)` ack-claim + single Recv-consumer router (Task 2, Д4).
///
/// The shared node owns ONE inbound event stream (the control-protocol accept
/// path). Before this, every role handle shared that stream, so an ack could be
/// delivered to the wrong sender engine which silently dropped it (audit C1). The
/// demux instead routes each decoded inbound event to exactly one consumer:
///
/// - `AnnounceReceived` / `Project*Received` → the single registered **Recv**
///   consumer (`handle(Role::Recv).events()`).
/// - `AckReceived { from, package_id }` → the **claimant** that registered
///   `(from, package_id)` when it announced (an Out/Collab handle's own channel).
///
/// An event with no matching claim/consumer is an **orphan**: logged and dropped
/// WITHOUT a delivery ack (so the sender retries), never misrouted. Claims are
/// released on the routed ack, an announce failure, or the owning handle's drop.
pub(crate) struct EventDemux {
    inner: Mutex<DemuxInner>,
}

#[derive(Default)]
struct DemuxInner {
    /// The single registered Recv consumer: `(owning handle id, sender)`.
    recv: Option<(u64, mpsc::Sender<TransportEvent>)>,
    /// Ack claims: `(peer, package_id)` → `(owning handle id, that handle's sender)`.
    claims: HashMap<(NodeId, PackageId), (u64, mpsc::Sender<TransportEvent>)>,
}

impl EventDemux {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(DemuxInner::default()),
        })
    }

    /// Register (or replace) the single Recv consumer. Only one Recv handle runs
    /// in practice; a later registration wins (last-consumer-wins).
    fn register_recv(&self, handle_id: u64, tx: mpsc::Sender<TransportEvent>) {
        self.inner.lock().expect("demux mutex poisoned").recv = Some((handle_id, tx));
    }

    /// Register an ack claim for `(peer, package_id)` pointing at the announcing
    /// handle's own events channel.
    fn register_claim(
        &self,
        handle_id: u64,
        key: (NodeId, PackageId),
        tx: mpsc::Sender<TransportEvent>,
    ) {
        self.inner
            .lock()
            .expect("demux mutex poisoned")
            .claims
            .insert(key, (handle_id, tx));
    }

    /// Release a single ack claim (announce-failure path; the delivery path
    /// consumes its own claim inside [`deliver_inbound`](Self::deliver_inbound)).
    fn release_claim(&self, key: &(NodeId, PackageId)) {
        self.inner
            .lock()
            .expect("demux mutex poisoned")
            .claims
            .remove(key);
    }

    /// Drop every registration owned by a handle that is going away (its `Drop`),
    /// so a claim/consumer can never outlive its handle and leak.
    fn release_handle(&self, handle_id: u64) {
        let mut inner = self.inner.lock().expect("demux mutex poisoned");
        inner.claims.retain(|_, (owner, _)| *owner != handle_id);
        if matches!(inner.recv, Some((id, _)) if id == handle_id) {
            inner.recv = None;
        }
    }

    /// Route one decoded inbound accept-path event to its consumer, returning how
    /// the accept loop should proceed (see [`Delivery`]). Called from the shared
    /// [`EventSink::Demux`] arm in the parent module.
    pub(crate) async fn deliver_inbound(&self, event: TransportEvent) -> Delivery {
        // Resolve the target sender under the lock (consuming an ack's claim, so a
        // claim is released the moment its ack routes), then send without the lock.
        let target = {
            let mut inner = self.inner.lock().expect("demux mutex poisoned");
            match &event {
                TransportEvent::AckReceived { from, package_id, .. } => inner
                    .claims
                    .remove(&(*from, package_id.clone()))
                    .map(|(_, tx)| tx),
                TransportEvent::AnnounceReceived { .. }
                | TransportEvent::ProjectAnnounceReceived { .. }
                | TransportEvent::ProjectRequestReceived { .. } => {
                    inner.recv.as_ref().map(|(_, tx)| tx.clone())
                }
                // ServeProgress / ServeComplete originate on OUR endpoint (the
                // provider-events consumer), never arrive as a decoded inbound
                // control message, and are routed via `route_serve_progress` /
                // `route_serve_complete` — not this path. Treat a stray one
                // defensively as an orphan (no consumer, no delivery ack).
                TransportEvent::ServeProgress { .. } | TransportEvent::ServeComplete { .. } => None,
            }
        };
        match target {
            Some(tx) => match tx.send(event).await {
                Ok(()) => Delivery::Delivered,
                Err(_) => Delivery::ConsumerGone,
            },
            None => {
                let (kind, peer) = event_kind_and_peer(&event);
                tracing::warn!(kind, peer = %hex32(&peer), "inbound event with no consumer");
                Delivery::Orphan
            }
        }
    }

    /// Number of live ack claims. Consumed by the node's idle gate (Task 8: a
    /// node with an outstanding announce-ack claim is NOT idle — a rebuild would
    /// break the connection the peer's ack rides back on) and by test
    /// introspection.
    fn claim_count(&self) -> usize {
        self.inner.lock().expect("demux mutex poisoned").claims.len()
    }

    /// Route a locally-generated [`ServeProgress`](TransportEvent::ServeProgress)
    /// (Task 13) to the sender handle(s) that announced this package — matched by
    /// `package_id` across the live ack claims. A claim points at the announcing
    /// Out/Collab handle's own events channel and lives exactly for the transfer
    /// window (registered at announce, released on the ack), so a tick routes to
    /// that sender engine only while its transfer is in flight. Non-blocking
    /// (`try_send`): a full channel drops the tick, and a package with no live
    /// claim (already acked / terminal, or announced by a since-dropped handle) is
    /// silently ignored — upload progress is best-effort UI data.
    pub(crate) fn route_serve_progress(&self, package_id: &PackageId, bytes_sent: u64) {
        let inner = self.inner.lock().expect("demux mutex poisoned");
        // TODO(sync-queue follow-up): keyed on `package_id` alone, so a package
        // fanned out to N peers (N same-`package_id` claims, distinguished only by
        // `(peer, package_id)`) delivers this SAME cumulative-bytes tick to every
        // one of them — a mild cross-destination over-report (never a misroute:
        // each claim still only ever sees ITS OWN transfer's real acks). Precise
        // per-destination attribution needs the provider event's `connection_id`
        // correlated to a peer via `ClientConnectedNotify.endpoint_id` (not
        // threaded through today); out of scope for Task 13.
        for ((_, pid), (_, tx)) in inner.claims.iter() {
            if pid == package_id {
                let _ = tx.try_send(TransportEvent::ServeProgress {
                    package_id: package_id.clone(),
                    bytes_sent,
                });
            }
        }
    }

    /// Route a locally-generated [`ServeComplete`](TransportEvent::ServeComplete)
    /// (Task 2.1) to the sender handle(s) that announced this package — the
    /// terminal-success clone of [`route_serve_progress`](Self::route_serve_progress),
    /// matched by `package_id` across the live ack claims. A claim lives exactly
    /// for the transfer window (registered at announce, released on the ack), so a
    /// complete routes to that sender engine only while its transfer is in flight.
    /// Non-blocking (`try_send`): a full channel drops it, and a package with no
    /// live claim (already acked / terminal, or announced by a since-dropped
    /// handle) is silently ignored — upload-complete is best-effort UI signalling.
    /// (Same keyed-on-`package_id`-only cross-destination caveat as
    /// `route_serve_progress` above — a benign over-report to N same-id claims,
    /// never a misroute.)
    pub(crate) fn route_serve_complete(&self, package_id: &PackageId) {
        let inner = self.inner.lock().expect("demux mutex poisoned");
        for ((_, pid), (_, tx)) in inner.claims.iter() {
            if pid == package_id {
                let _ = tx.try_send(TransportEvent::ServeComplete {
                    package_id: package_id.clone(),
                });
            }
        }
    }
}

/// The inbound-event variant an orphan warn names, plus the peer it came from.
fn event_kind_and_peer(event: &TransportEvent) -> (&'static str, NodeId) {
    match event {
        TransportEvent::AnnounceReceived { from, .. } => ("announce", *from),
        TransportEvent::AckReceived { from, .. } => ("ack", *from),
        TransportEvent::ProjectAnnounceReceived { from, .. } => ("project_announce", *from),
        TransportEvent::ProjectRequestReceived { from, .. } => ("project_request", *from),
        // Locally-originated (no peer); never reach the inbound orphan path.
        TransportEvent::ServeProgress { .. } => ("serve_progress", [0u8; 32]),
        TransportEvent::ServeComplete { .. } => ("serve_complete", [0u8; 32]),
    }
}

/// Per-peer pooled control connection (Task 2): one dialed QUIC connection per
/// peer node id, reused across control messages, idle-closed after
/// [`CONTROL_POOL_IDLE`]. Replaces the previous connect-per-message idiom so a
/// burst of announces/acks rides one connection (and spawns the path-diagnostics
/// watcher once, not per message). Any send error invalidates the entry so the
/// next send re-dials.
struct ControlPool {
    endpoint: Endpoint,
    entries: Mutex<HashMap<NodeId, PoolEntry>>,
    /// Count of real dials (test introspection: pooled reuse keeps this flat).
    dials: AtomicU64,
}

struct PoolEntry {
    conn: Connection,
    /// Updated on every reuse; the idle reaper closes the connection once it has
    /// been untouched for [`CONTROL_POOL_IDLE`].
    last_used: Arc<Mutex<Instant>>,
}

impl ControlPool {
    fn new(endpoint: Endpoint) -> Arc<Self> {
        Arc::new(Self {
            endpoint,
            entries: Mutex::new(HashMap::new()),
            dials: AtomicU64::new(0),
        })
    }

    /// A live pooled connection to `to`: reused if one is open, else freshly
    /// dialed (spawning its path-diagnostics watcher + idle reaper once).
    async fn get_or_connect(
        self: &Arc<Self>,
        to: NodeId,
        target: EndpointAddr,
    ) -> Result<Connection> {
        {
            let entries = self.entries.lock().expect("control_pool mutex poisoned");
            if let Some(entry) = entries.get(&to) {
                if entry.conn.close_reason().is_none() {
                    *entry.last_used.lock().expect("last_used mutex poisoned") = Instant::now();
                    return Ok(entry.conn.clone());
                }
            }
        }
        let conn = self
            .endpoint
            .connect(target, SYNC_ALPN)
            .await
            .context("connect sync control channel")?;
        self.dials.fetch_add(1, Ordering::Relaxed);
        // Once per pooled connection (not per message): the establishment line +
        // the relay→direct path-upgrade watcher.
        spawn_conn_path_diagnostics(&conn, "outgoing");
        let last_used = Arc::new(Mutex::new(Instant::now()));
        {
            let mut entries = self.entries.lock().expect("control_pool mutex poisoned");
            entries.insert(
                to,
                PoolEntry {
                    conn: conn.clone(),
                    last_used: Arc::clone(&last_used),
                },
            );
        }
        Arc::clone(self).spawn_idle_reaper(to, conn.clone(), last_used);
        Ok(conn)
    }

    /// Drop the pooled entry for `to` after a send error — but only if it still
    /// holds `conn` (never evict a newer re-dial), so the next send re-dials.
    fn invalidate(&self, to: NodeId, conn: &Connection) {
        let mut entries = self.entries.lock().expect("control_pool mutex poisoned");
        if entries.get(&to).map(|e| e.conn.stable_id()) == Some(conn.stable_id()) {
            entries.remove(&to);
        }
    }

    /// Close + evict a pooled connection once it has been idle for
    /// [`CONTROL_POOL_IDLE`]. One task per pooled connection; holds only a `Weak`
    /// to the pool so it never keeps it alive, and exits after it fires.
    fn spawn_idle_reaper(self: Arc<Self>, to: NodeId, conn: Connection, last_used: Arc<Mutex<Instant>>) {
        let stable_id = conn.stable_id();
        let weak = Arc::downgrade(&self);
        drop(self);
        tokio::spawn(async move {
            loop {
                let deadline =
                    *last_used.lock().expect("last_used mutex poisoned") + CONTROL_POOL_IDLE;
                let now = Instant::now();
                if now < deadline {
                    tokio::time::sleep(deadline - now).await;
                    continue;
                }
                conn.close(0u32.into(), b"idle");
                if let Some(pool) = weak.upgrade() {
                    let mut entries = pool.entries.lock().expect("control_pool mutex poisoned");
                    if entries.get(&to).map(|e| e.conn.stable_id()) == Some(stable_id) {
                        entries.remove(&to);
                    }
                }
                tracing::debug!(peer = %hex32(&to), "pooled control connection closed after idle");
                break;
            }
        });
    }
}

/// The rebuildable network layer of a [`SharedIrohNode`] (Task 8, H2). Everything
/// a relay-map change replaces lives here behind one lock, so an idle rebuild can
/// swap the whole set atomically WITHOUT invalidating the `Arc<SharedIrohNode>` or
/// any role handle: the endpoint + router are re-bound (new relay mode, same
/// device secret ⇒ same node id) and the control pool + relay watcher rebuilt on
/// them, while the store, demux, gate, responder, peers, key lock, and node id
/// (all outside this struct) are preserved untouched across the swap.
struct NetLayer {
    /// Endpoint handle (clone of the router's); dials peers + downloads blobs.
    endpoint: Endpoint,
    /// Keeps the accept loop (both protocols) alive; torn down in
    /// [`shutdown`](SharedIrohNode::shutdown), replaced on rebuild. `Router` is
    /// `Clone` (Arc-backed accept task), so shutdown clones it out of the lock.
    router: Router,
    /// Per-peer pooled control connections (Task 2), reused across control sends.
    /// Rebound (dropped + freshly built) on the new endpoint at rebuild.
    control_pool: Arc<ControlPool>,
    /// Home-relay status watcher task on this endpoint; aborted at shutdown and at
    /// rebuild (iroh keeps the watcher alive until the last endpoint clone drops).
    relay_watcher: Option<JoinHandle<()>>,
    /// The relay URLs the endpoint was built with (H1 reporting groundwork, T7).
    relay_urls: Vec<String>,
    /// Whether a relay is configured. Gates the `online()` wait — with the relay
    /// disabled there is no home relay and `online()` would hang.
    uses_relay: bool,
}

/// The one iroh endpoint/router/store for this process (see module docs).
pub struct SharedIrohNode {
    /// The rebuildable endpoint/router/control-pool/relay-config (Task 8). Behind
    /// one lock so a relay-map rebuild swaps the whole set atomically; hot readers
    /// (`endpoint`/`control_pool` accessors) clone the cheap Arc-backed handle out
    /// and never hold the lock across an await.
    net: Mutex<NetLayer>,
    /// The single blob store shared by every role handle. NOT rebuilt on a relay
    /// change (only the endpoint/router are) — a re-open would risk the redb lock.
    store: Store,
    /// This endpoint's node id (== ed25519 public key bytes). STABLE across a
    /// relay rebuild (same device secret is re-bound), so peers and history keep
    /// addressing this node by the same id.
    node_id: NodeId,
    /// The device secret the endpoint binds, kept so a relay rebuild can re-bind
    /// the SAME identity without touching the key file or its advisory lock.
    device_secret: [u8; 32],
    /// Known peer addresses (from pairing), used to dial the control channel.
    peers: Mutex<HashMap<NodeId, EndpointAddr>>,
    /// Endpoint address lookup — consumed by the blobs downloader when it dials
    /// by node id. Cloned handle; shares state with the endpoint. Reused (its
    /// learned peer info preserved) across a rebuild by re-binding the new
    /// endpoint against this same instance.
    lookup: iroh::address_lookup::memory::MemoryLookup,
    /// Prefixed package tag (`<role>/pkg/<id>`) → collection hash, registered by
    /// [`serve`](SharingTransport::serve), injected into the wire announce by
    /// [`announce`](SharingTransport::announce). Keyed by the FULL prefixed tag
    /// so two roles serving the same package id never collide. Behind an `Arc` so
    /// the provider-upload-events consumer's [`ServeRootResolver`] (Task 13) can
    /// share it — the consumer is spawned at router build, before `Self` exists.
    served: Arc<Mutex<HashMap<String, Hash>>>,
    /// Per-`(peer, package)` ack-claim + Recv-consumer router (Task 2, Д4). Owns
    /// the fan-out of the node's single inbound event stream; cloned into the
    /// control-protocol handler (as [`EventSink::Demux`]) and consulted by every
    /// role handle's [`events`](SharingTransport::events) /
    /// [`announce`](SharingTransport::announce). Preserved across a rebuild — the
    /// new control handler references the SAME demux, so consumers stay registered.
    demux: Arc<EventDemux>,
    /// Count of in-flight serve/fetch/announce/ack operations (Task 8 idle gate).
    /// A relay rebuild runs only at `active_ops == 0` AND zero demux claims, so it
    /// never tears the endpoint out from under a live transfer.
    active_ops: AtomicU64,
    /// Number of relay rebuilds that have executed (test/observability
    /// introspection — the T7 reporter + handles are meant to survive each one).
    rebuild_count: AtomicU64,
    /// Monotonic id source for role handles, so the demux can attribute a claim /
    /// the Recv consumer to a specific handle and release them all on its drop.
    next_handle_id: AtomicU64,
    /// Connection-level authorization gate, cloned into both protocol handlers;
    /// the host installs the predicate via [`set_connect_gate`](Self::set_connect_gate).
    /// Preserved across a rebuild (re-cloned into the new handlers).
    connect_gate: SharedConnectGate,
    /// Dedup responder slot, cloned into the control protocol handler; the
    /// receiver installs it via [`set_dedup_responder`](Self::set_dedup_responder)
    /// when it migrates onto the node (Task 3). Empty ⇒ inbound offers are
    /// answered want-all, so nothing is ever silently withheld. Preserved across a
    /// rebuild (re-cloned into the new handler).
    responder: SharedResponder,
    /// Role prefixes whose startup sweep has already run (once per prefix).
    swept: Mutex<HashSet<&'static str>>,
    /// Whether the one-shot home-relay `online()` wait has run. On success it
    /// seeds `relay_health` (connected + home-relay url) so `transport_health`
    /// reports `relay_connected` immediately — a BRIDGE until the watcher records
    /// its first transition, after which the watcher is the sole authority (Task
    /// 3.3).
    online_waited: AtomicBool,
    /// Whether [`shutdown`](Self::shutdown) has already run (idempotency guard).
    shutdown_done: AtomicBool,
    /// Immediate relay-refresh trigger (H2): the sign-in path calls
    /// [`request_relay_refresh`](Self::request_relay_refresh) to wake the loop now
    /// instead of waiting for the hourly tick.
    refresh_notify: Arc<Notify>,
    /// A relay change awaiting an idle rebuild (H2): the target relay mode plus how
    /// long it has been deferred (for the [`RELAY_REBUILD_MAX_DEFER`] force).
    pending_rebuild: Mutex<Option<PendingRebuild>>,
    /// The relay-url set the last resolver run reported — the change-detection
    /// baseline (H2). A resolver reporting a DIFFERENT set marks a pending rebuild;
    /// initialized to the bind-time relay set so the first unchanged resolve is a
    /// no-op.
    last_relay_urls: Mutex<Vec<String>>,
    /// The relay-refresh loop task; aborted at shutdown so no rebuild starts
    /// mid-teardown. `None` until [`start_relay_refresh`](Self::start_relay_refresh)
    /// runs (once; idempotent).
    refresh_task: Mutex<Option<JoinHandle<()>>>,
    /// The held device-key advisory lock (I4); dropped at shutdown so a re-bind
    /// can re-acquire it. Kept HELD across a relay rebuild — the identity never
    /// changes, so the lock is neither released nor re-acquired.
    key_lock: Mutex<Option<DeviceKeyLock>>,
    /// Transport-level wake hook (Task 6): fired on a home-relay reconnect
    /// transition and after an applied relay-map change, so the host kicks every
    /// pending outbound package. Behind an `Arc<RwLock<…>>` so the relay watcher —
    /// respawned on every relay rebuild — reads the CURRENT hook live at fire time
    /// (survives a rebuild). Never invoked while the lock is held: the value is
    /// cloned out first, so a hook that re-enters the node can't self-deadlock.
    wake_hook: Arc<RwLock<Option<WakeHook>>>,
    /// The home-relay watcher's last-recorded transition (Task 3.3). Like
    /// [`wake_hook`](Self::wake_hook) it lives on this rebuild-surviving outer
    /// layer and is handed to the watcher task, which IS respawned on every relay
    /// rebuild — so the watcher writes into the SAME cell across rebuilds and
    /// [`transport_health`](Self::transport_health) always reads the current
    /// picture. Read under the lock and cloned out; never held across an await.
    relay_health: Arc<RwLock<RelayHealth>>,
}

/// A relay change awaiting an idle rebuild (Task 8, H2).
struct PendingRebuild {
    /// The relay mode the endpoint should be re-bound with.
    mode: RelayMode,
    /// When this rebuild first became pending — drives the
    /// [`RELAY_REBUILD_MAX_DEFER`] force.
    since: Instant,
    /// Whether the "deferred beyond max" `warn!` has already fired (once per
    /// pending rebuild, not once per retry poll).
    warned: bool,
}

/// RAII marker for one in-flight transport operation (Task 8 idle gate).
/// Increments the node's `active_ops` on construction, decrements on drop, so an
/// idle-gated relay rebuild never fires while a serve/fetch/announce is running.
struct OpGuard<'a> {
    counter: &'a AtomicU64,
}

impl Drop for OpGuard<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

impl SharedIrohNode {
    /// Bind the ONE endpoint for this process from the install's device key.
    ///
    /// Takes the device-key advisory lock (fail ⇒ actionable, key-material-free
    /// error, I4/S2); builds the endpoint (`presets::Minimal` + secret +
    /// `relay_mode` + [`MemoryLookup`](iroh::address_lookup::memory::MemoryLookup));
    /// opens the single [`FsStore`] at `<sync_dir>/blobs` (GC enabled once);
    /// mounts the [`Router`] with both ALPNs once; spawns the home-relay-status
    /// watcher. `relay_mode` is [`RelayMode::Default`] for production or
    /// [`RelayMode::Disabled`] for direct-only / in-process tests.
    pub async fn bind(sync_dir: &Path, relay_mode: RelayMode) -> Result<Arc<Self>> {
        // The one device identity (task B4): the endpoint binds this secret and
        // the advisory lock is on this exact file, so a second install started
        // from a copied key fails here instead of dueling on the relay (I4).
        let key = DeviceKey::load_or_create_in(sync_dir).context("load device key")?;
        let key_lock = DeviceKeyLock::acquire(&device_key_path(sync_dir))?;

        let demux = EventDemux::new();
        // Keep the raw secret so a relay rebuild can re-bind the SAME identity
        // (same node id) without touching the key file or its held advisory lock.
        let device_secret = key.secret_bytes();
        let secret_key = SecretKey::from_bytes(&device_secret);
        let uses_relay = !matches!(relay_mode, RelayMode::Disabled);
        // Snapshot what the ENDPOINT is being built with before the builder
        // consumes `relay_mode` (same transport-level view as `IrohTransport`).
        let relay_mode_label = match &relay_mode {
            RelayMode::Disabled => "disabled",
            RelayMode::Default => "default",
            RelayMode::Staging => "staging",
            RelayMode::Custom(_) => "custom",
        };
        let relay_map = relay_mode.relay_map();
        let relay_count = relay_map.len();
        let relay_urls: Vec<String> = relay_map
            .urls::<Vec<_>>()
            .iter()
            .map(|u| u.to_string())
            .collect();
        let lookup = iroh::address_lookup::memory::MemoryLookup::new();

        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(secret_key)
            .relay_mode(relay_mode)
            .address_lookup(lookup.clone())
            .bind()
            .await
            .context("bind iroh endpoint")?;
        tracing::info!(
            relay_mode = relay_mode_label,
            relay_count,
            node_id = %endpoint.id().fmt_short(),
            "shared iroh node relay configuration"
        );

        // One `FsStore` at `<sync_dir>/blobs` for all roles (spec §2). Mirror
        // `FsStore::load`'s internals but with GC on (load() hardcodes gc: None,
        // so released blobs would leak forever). Interval is slack (see
        // `GC_INTERVAL`) so an in-flight transfer never races collection.
        let blob_dir = sync_dir.join("blobs");
        std::fs::create_dir_all(&blob_dir)
            .with_context(|| format!("create blob dir {}", blob_dir.display()))?;
        let db_path = blob_dir.join("blobs.db");
        let mut options = FsOptions::new(&blob_dir);
        options.gc = Some(GcConfig {
            interval: GC_INTERVAL,
            add_protected: None,
        });
        let store: Store = FsStore::load_with_opts(db_path, options)
            .await
            .with_context(|| format!("open blob store {}", blob_dir.display()))?
            .into();

        // Shared connect gate: unset at construction, installed later by the host
        // (`set_connect_gate`). Cloned into BOTH handlers so a single install
        // point governs the control channel AND the blobs provider (S4 parity
        // with `IrohTransport`).
        let connect_gate: SharedConnectGate = Arc::new(Mutex::new(None));
        // Late-bindable dedup responder slot (Д3 shape): empty at bind, installed
        // by the receiver when it migrates onto the node (`set_dedup_responder`,
        // Task 3). Empty ⇒ offers are answered want-all, so nothing is ever
        // silently withheld before the receiver wires its catalog responder.
        let responder: SharedResponder = Arc::new(Mutex::new(None));
        // The served map lives behind an `Arc` so the provider-upload-events
        // consumer (Task 13) can resolve a served collection's root hash back to its
        // package id. Created here — before `Self` — and shared into both the
        // resolver and the struct field below.
        let served: Arc<Mutex<HashMap<String, Hash>>> = Arc::new(Mutex::new(HashMap::new()));
        let serve_resolver: ServeRootResolver = {
            let served = Arc::clone(&served);
            Arc::new(move |hash| resolve_served_root_in(&served, hash))
        };
        // Shared node: `EventSink::Demux` (inbound events fan out through the demux,
        // Task 2/Д4, not a single shared stream), and `flush_store_on_shutdown:
        // false` because the store is SHARED across relay rebuilds — a per-rebuild
        // router teardown must not tear it down; the node flushes it explicitly in
        // `shutdown` (T8).
        let router = build_router(
            endpoint,
            &store,
            &connect_gate,
            EventSink::Demux(Arc::clone(&demux)),
            Arc::clone(&responder),
            false,
            serve_resolver,
        );

        let endpoint = router.endpoint().clone();
        let node_id: NodeId = *endpoint.id().as_bytes();
        let control_pool = ControlPool::new(endpoint.clone());
        // The wake hook lives outside `NetLayer` (unaffected by a relay rebuild)
        // but its Arc is handed to the watcher task, which IS respawned on rebuild —
        // so the watcher reads whatever hook the api layer has installed at fire
        // time, not a stale clone captured here at bind.
        let wake_hook: Arc<RwLock<Option<WakeHook>>> = Arc::new(RwLock::new(None));
        // The relay-health cell also lives outside `NetLayer` (survives a relay
        // rebuild) and is handed to the watcher, which IS respawned on rebuild —
        // so the watcher writes the CURRENT node's transitions into this same cell.
        let relay_health: Arc<RwLock<RelayHealth>> = Arc::new(RwLock::new(RelayHealth::initial()));
        let relay_watcher = spawn_home_relay_watcher(
            endpoint.clone(),
            Arc::clone(&wake_hook),
            Arc::clone(&relay_health),
        );

        tracing::debug!(node_id = %endpoint.id().fmt_short(), "shared iroh node bound");
        Ok(Arc::new(Self {
            net: Mutex::new(NetLayer {
                endpoint,
                router,
                control_pool,
                relay_watcher: Some(relay_watcher),
                relay_urls: relay_urls.clone(),
                uses_relay,
            }),
            store,
            node_id,
            device_secret,
            peers: Mutex::new(HashMap::new()),
            lookup,
            served,
            demux,
            active_ops: AtomicU64::new(0),
            rebuild_count: AtomicU64::new(0),
            next_handle_id: AtomicU64::new(0),
            connect_gate,
            swept: Mutex::new(HashSet::new()),
            online_waited: AtomicBool::new(false),
            shutdown_done: AtomicBool::new(false),
            refresh_notify: Arc::new(Notify::new()),
            pending_rebuild: Mutex::new(None),
            // The change-detection baseline starts at the bind-time relay set, so
            // the first resolver run that reports the SAME set is a no-op.
            last_relay_urls: Mutex::new(relay_urls),
            refresh_task: Mutex::new(None),
            key_lock: Mutex::new(Some(key_lock)),
            responder,
            wake_hook,
            relay_health,
        }))
    }

    // ----- rebuildable network-layer accessors (Task 8) -----------------------

    /// A clone of the current endpoint handle (cheap, Arc-backed). Never holds the
    /// `net` lock across an await — callers use the returned clone.
    fn endpoint(&self) -> Endpoint {
        self.net.lock().expect("net mutex poisoned").endpoint.clone()
    }

    /// A clone of the current pooled-control-connection handle.
    fn control_pool(&self) -> Arc<ControlPool> {
        Arc::clone(&self.net.lock().expect("net mutex poisoned").control_pool)
    }

    /// Whether a relay is configured on the CURRENT endpoint (gates the `online()`
    /// wait). Reads through the rebuildable layer so a relay rebuild is reflected.
    fn uses_relay(&self) -> bool {
        self.net.lock().expect("net mutex poisoned").uses_relay
    }

    /// An RAII guard that marks one in-flight serve/fetch/announce/ack operation
    /// (Task 8 idle gate); a relay rebuild defers while any are outstanding.
    fn op_guard(&self) -> OpGuard<'_> {
        self.active_ops.fetch_add(1, Ordering::SeqCst);
        OpGuard { counter: &self.active_ops }
    }

    /// Whether the node is idle enough to rebuild the endpoint (Task 8): no
    /// in-flight transport operation AND no outstanding announce-ack claim (a
    /// rebuild would break the connection a pending ack rides back on).
    fn is_idle(&self) -> bool {
        self.active_ops.load(Ordering::SeqCst) == 0 && self.demux.claim_count() == 0
    }

    /// This endpoint's node id (== ed25519 public key bytes).
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Resolve a served collection's root hash to the [`PackageId`] whose
    /// [`serve`](SharingTransport::serve) registered it (Task 13). The named
    /// reverse-resolution entry point + test hook; the provider-upload-events
    /// consumer resolves via a [`ServeRootResolver`] closure over the SAME shared
    /// map (it is spawned before `Self` exists, so it can't hold `&self`). `None`
    /// for a child blob / hash-seq-internal / foreign hash.
    #[allow(dead_code)] // exposed contract; the consumer resolves via the shared closure
    pub(crate) fn resolve_served_root(&self, hash: Hash) -> Option<PackageId> {
        resolve_served_root_in(&self.served, hash)
    }

    /// Live ack-claim count (test introspection for the demux).
    #[cfg(test)]
    pub(crate) fn active_claims(&self) -> usize {
        self.demux.claim_count()
    }

    /// Number of real control-connection dials (test introspection: pooled reuse
    /// keeps this flat across sends to the same peer).
    #[cfg(test)]
    pub(crate) fn control_pool_dials(&self) -> u64 {
        self.control_pool().dials.load(Ordering::Relaxed)
    }

    /// Number of relay rebuilds that have executed (Task 8 introspection).
    #[cfg(test)]
    pub(crate) fn rebuild_count(&self) -> u64 {
        self.rebuild_count.load(Ordering::SeqCst)
    }

    /// Test-only: overwrite the home-relay health cell to simulate a watcher
    /// transition (or the online-wait seed) without a live relay, so the
    /// `transport_health` derivation can be exercised deterministically (all
    /// loopback binds are relay-disabled). Task 3.3 regression coverage.
    #[cfg(test)]
    pub(crate) fn set_relay_health_for_test(&self, health: RelayHealth) {
        *self.relay_health.write().expect("relay_health lock poisoned") = health;
    }

    /// Test-only: force `uses_relay` on the current net layer so the health
    /// derivation exercises the relay-configured branch without binding a real
    /// relay endpoint (which would do network I/O). Task 3.3 regression coverage.
    #[cfg(test)]
    pub(crate) fn force_uses_relay_for_test(&self, uses_relay: bool) {
        self.net.lock().expect("net mutex poisoned").uses_relay = uses_relay;
    }

    /// This endpoint's current [`EndpointAddr`] (direct addrs + relay url) — the
    /// self-reported address for H1 (Task 7). Call after a handle's
    /// [`start`](SharingTransport::start) so address discovery has settled. Reads
    /// through the rebuildable layer, so the T7 reporter that polls this survives
    /// a relay rebuild and reports the NEW endpoint's address.
    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.endpoint().addr()
    }

    /// The relay URLs the current endpoint was built with (H1 groundwork, Task 7).
    pub fn relay_urls(&self) -> Vec<String> {
        self.net.lock().expect("net mutex poisoned").relay_urls.clone()
    }

    /// The node's current transport-reachability health for the status poll
    /// (Task 3.3), combining two signals with NO network I/O: whether a relay is
    /// configured on the current endpoint ([`uses_relay`](Self::uses_relay)) and
    /// the [`RelayHealth`] cell — written by the home-relay watcher on every
    /// transition, and seeded once by the successful one-shot `online()` wait as a
    /// bridge before the watcher's first transition. The cell is the SOLE
    /// connected-ness authority: once the watcher records a disconnect the node
    /// correctly reads `direct_only`, so a dropped relay always flips the surface
    /// (the earlier `online_ok` latch masked this — it never reset). A bound node
    /// reports only [`relay_connected`](TransportHealth::relay_connected) or
    /// [`direct_only`](TransportHealth::direct_only) — `not_started` (no node) and
    /// `no_relay_map` (signed in, no relay configuration) are api-layer
    /// derivations in [`crate::api::sync::derive_transport_health`], which owns the
    /// sign-in / cached-relay state this layer does not see.
    pub fn transport_health(&self) -> TransportHealth {
        // Relay disabled at bind (direct-only / loopback): there is no home relay,
        // so the node is undialable by peers behind NAT.
        if !self.uses_relay() {
            return TransportHealth::direct_only(None, None);
        }
        let health = self.relay_health.read().expect("relay_health lock poisoned").clone();
        // The cell (watcher transitions, online-wait-seeded before the first one)
        // is the sole authority. It starts disconnected, so a freshly-bound node
        // reads `direct_only` until the relay connects — and reverts to it the
        // moment the watcher records a disconnect.
        if health.connected {
            TransportHealth::relay_connected(health.url)
        } else {
            TransportHealth::direct_only(health.url, health.last_error)
        }
    }

    /// Acquire a role handle (Д3): a [`SharingTransport`] that role-prefixes its
    /// blob tags and, on [`start`](SharingTransport::start), sweeps ONLY its own
    /// prefix. All handles share the node's single endpoint/router/store.
    pub fn handle(self: &Arc<Self>, role: Role) -> Arc<dyn SharingTransport> {
        self.role_handle(role)
    }

    /// Concrete [`RoleHandle`] variant of [`handle`](Self::handle) — used within
    /// the crate (and tests) when the concrete type's helpers (e.g.
    /// [`RoleHandle::store`]) are needed.
    pub(crate) fn role_handle(self: &Arc<Self>, role: Role) -> Arc<RoleHandle> {
        let id = self.next_handle_id.fetch_add(1, Ordering::Relaxed);
        Arc::new(RoleHandle {
            node: Arc::clone(self),
            role,
            id,
            events_tx: Mutex::new(None),
        })
    }

    /// Install the connection-level authorization [`ConnectGate`] (governs BOTH
    /// ALPNs, S4). Overwrites any previously installed gate; left unset, the node
    /// admits every connection.
    pub fn set_connect_gate(&self, gate: ConnectGate) {
        *self.connect_gate.lock().expect("connect_gate mutex poisoned") = Some(gate);
    }

    /// Install the dedup [`DedupResponder`] that answers inbound `Offer` /
    /// `FullHashes` handshakes on the control channel (Task 3 receiver migration).
    /// The running receiver installs a `CatalogDedupResponder` here so a peer's
    /// pre-Announce dedup negotiation is answered from our catalog; left unset,
    /// the node answers offers want-all (nothing silently withheld). Overwrites
    /// any previously installed responder; picked up live by the already-spawned
    /// control handler on the next inbound offer.
    pub fn set_dedup_responder(&self, responder: Arc<dyn DedupResponder>) {
        *self.responder.lock().expect("responder mutex poisoned") = Some(responder);
    }

    /// Install the transport-level [`WakeHook`] (Task 6): fired on a home-relay
    /// reconnect transition and after an applied relay-map change, so the host can
    /// kick every pending outbound package the moment the node is reachable again.
    /// Overwrites any previously installed hook; picked up live by the
    /// already-spawned relay watcher (it reads the hook at fire time) and by the
    /// refresh loop. Left unset, both wake sources are silent no-ops.
    pub fn set_wake_hook(&self, hook: WakeHook) {
        *self.wake_hook.write().expect("wake_hook lock poisoned") = Some(hook);
    }

    /// Fire the installed wake hook, if any. Clones the hook out from under the
    /// read lock FIRST, then releases the lock and invokes it — so a hook that
    /// re-enters the node (or races [`set_wake_hook`](Self::set_wake_hook)) can
    /// never deadlock on the wake-hook lock. A no-op when no hook is installed.
    fn fire_wake_hook(&self) {
        let hook = self
            .wake_hook
            .read()
            .expect("wake_hook lock poisoned")
            .clone();
        if let Some(h) = hook {
            h();
        }
    }

    /// Register a peer's dialable address (from a pairing ticket or hub report),
    /// enabling the control channel and the blobs downloader to reach it.
    pub fn add_peer(&self, addr: EndpointAddr) {
        let node: NodeId = *addr.id.as_bytes();
        self.lookup.add_endpoint_info(addr.clone());
        self.peers
            .lock()
            .expect("peers mutex poisoned")
            .insert(node, addr);
    }

    /// Merge a relay dial hint for an inbound peer we're about to pull blobs from
    /// into the endpoint's address lookup (finding H1 / I2, T7). Built from OUR
    /// own relay set — same-account peers ride the same hub relays — so an inbound
    /// peer with no cached direct path still gets a relay route for the blob pull.
    /// Written ONLY into the address lookup the blobs downloader dials through,
    /// never the control-dial `peers` map, and as a MERGE (`add_endpoint_info`),
    /// so it can never downgrade a richer address the node already knows. An empty
    /// relay set yields a bare addr and is a no-op — exactly the pre-T7 behavior.
    pub fn add_peer_dial_hint(&self, from: NodeId) {
        let relay_urls = self.relay_urls();
        match crate::sync::pairing::peer_addr_with_relays(from, &relay_urls) {
            Ok(addr) if !addr.is_empty() => self.lookup.add_endpoint_info(addr),
            Ok(_) => {} // no relays resolved → nothing to hint (current behavior)
            Err(e) => tracing::warn!(
                error = %format!("{e:#}"),
                peer = %hex32(&from),
                "add_peer_dial_hint: address build failed"
            ),
        }
    }

    /// Parse a peer's pairing ticket ([`StartInfo::pairing_ticket`]) and register
    /// its address. Idempotent.
    pub fn add_peer_ticket(&self, ticket: &str) -> Result<()> {
        let ticket: EndpointTicket = ticket.parse().context("parse peer pairing ticket")?;
        self.add_peer(ticket.endpoint_addr().clone());
        Ok(())
    }

    /// The node's single blob store, shared by every role handle (used by tests
    /// + the T3 migration's orphaned-dir cleanup).
    #[allow(dead_code)] // consumed by tests here and by the Task 3 store migration
    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    /// Gracefully tear down the node (I1): abort the relay refresh loop + relay
    /// watcher, then `Router::shutdown().await` → store shutdown → bounded
    /// `endpoint.close()`, then release the device-key lock. Idempotent — a second
    /// call is a no-op.
    pub async fn shutdown(&self) {
        if self.shutdown_done.swap(true, Ordering::SeqCst) {
            return;
        }
        // Abort the relay-refresh loop first (Task 8): aborting it cancels any
        // in-flight rebuild future at its next await, and the swap-time
        // `shutdown_done` check below closes the race if a rebuild already built a
        // new endpoint. Setting `shutdown_done` above (before this) means a
        // rebuild racing the lock discards its freshly-built endpoint.
        if let Some(handle) = self.refresh_task.lock().expect("refresh_task mutex poisoned").take() {
            handle.abort();
        }
        // Take the router + endpoint + relay watcher out of the rebuildable layer.
        // `Router` is `Clone` (shared accept task), so cloning it out of the lock
        // and shutting the clone down tears the one accept task down.
        let (router, endpoint, watcher) = {
            let mut net = self.net.lock().expect("net mutex poisoned");
            (net.router.clone(), net.endpoint.clone(), net.relay_watcher.take())
        };
        // Abort the relay watcher: iroh keeps the `home_relay_status` watcher alive
        // until the last endpoint clone drops (closing the endpoint does NOT stop
        // it), so it must be aborted explicitly.
        if let Some(handle) = watcher {
            handle.abort();
        }
        if let Err(e) = router.shutdown().await {
            tracing::warn!(error = %e, "shared iroh node router shutdown");
        }
        // Flush the persistent store to disk (also releases the redb file lock
        // so a later re-bind over the same dir can reopen it).
        if let Err(e) = self.store.shutdown().await {
            tracing::warn!(error = %e, "shared iroh node blob store shutdown");
        }
        if tokio::time::timeout(SHUTDOWN_CLOSE_TIMEOUT, endpoint.close())
            .await
            .is_err()
        {
            tracing::warn!(
                timeout_ms = SHUTDOWN_CLOSE_TIMEOUT.as_millis() as u64,
                "iroh endpoint close timed out"
            );
        }
        // Release the device-key advisory lock last — the identity stays
        // reserved until every teardown step has run; a re-bind then re-acquires.
        drop(self.key_lock.lock().expect("key_lock mutex poisoned").take());
        tracing::info!(node_id = %hex32(&self.node_id), "shared iroh node shut down");
    }

    // ----- relay-map lifecycle: refresh loop + idle rebuild (Task 8, H2) -------

    /// Start the hourly relay-map refresh loop (H2), bounding relay-map staleness.
    ///
    /// The loop re-runs the injected, hub-agnostic `resolver` on an hourly tick (or
    /// immediately when [`request_relay_refresh`](Self::request_relay_refresh) is
    /// called on the sign-in path). On a CHANGED relay-url set it marks a pending
    /// rebuild; the rebuild = a bounded internal endpoint/router re-bind executed
    /// only when the node is idle (no in-flight serve/fetch/announce, no
    /// outstanding ack claim), preserving the node id, store, and every role
    /// handle. A rebuild deferred past [`RELAY_REBUILD_MAX_DEFER`] is forced
    /// through regardless. Idempotent: a second call is a no-op (the first
    /// resolver wins), so the app's several start entry points can all call it.
    pub fn start_relay_refresh(self: &Arc<Self>, resolver: RelayResolver) {
        let mut guard = self.refresh_task.lock().expect("refresh_task mutex poisoned");
        if guard.is_some() {
            return;
        }
        let weak = Arc::downgrade(self);
        let notify = Arc::clone(&self.refresh_notify);
        let handle = tokio::spawn(async move {
            loop {
                // A shorter poll while a rebuild is pending-but-not-idle, so it
                // fires within seconds of the node going quiet; hourly otherwise.
                let pending = match weak.upgrade() {
                    Some(node) => node.pending_rebuild.lock().expect("pending mutex poisoned").is_some(),
                    None => return,
                };
                let wait = if pending {
                    RELAY_REBUILD_RETRY_INTERVAL
                } else {
                    RELAY_REFRESH_INTERVAL
                };
                // Re-run the resolver on the hourly tick or an immediate trigger;
                // the short pending-retry tick only re-checks the idle gate.
                let run_resolver = tokio::select! {
                    _ = tokio::time::sleep(wait) => !pending,
                    _ = notify.notified() => true,
                };
                let Some(node) = weak.upgrade() else { return };
                if node.shutdown_done.load(Ordering::SeqCst) {
                    return;
                }
                if run_resolver {
                    if let Some((mode, urls)) = resolver().await {
                        // A relay-map change means the node is (re)homing onto a
                        // fresh relay set — a wake event (Task 6): kick every
                        // pending outbound package so it re-announces on the new
                        // map instead of waiting out its backoff.
                        if node.consider_relay_change(mode, urls) {
                            node.fire_wake_hook();
                        }
                    }
                }
                node.try_rebuild().await;
            }
        });
        *guard = Some(handle);
    }

    /// Trigger an immediate relay-map refresh (the sign-in path): wake the refresh
    /// loop now instead of waiting for the hourly tick. A no-op if the loop is not
    /// running yet — the eventual [`start_relay_refresh`](Self::start_relay_refresh)
    /// will resolve at once on its first iteration.
    pub fn request_relay_refresh(&self) {
        self.refresh_notify.notify_one();
    }

    /// Fold a fresh resolver result into the change-detection baseline: on a
    /// CHANGED relay-url set, log + mark a pending rebuild for `mode`. Compares as
    /// sorted sets so relay-url ordering is not a spurious change. Returns `true`
    /// iff the set actually changed (a rebuild was marked) — the refresh loop uses
    /// that signal to fire the wake hook (Task 6).
    fn consider_relay_change(&self, mode: RelayMode, urls: Vec<String>) -> bool {
        let mut last = self.last_relay_urls.lock().expect("last_relay_urls mutex poisoned");
        let mut new_sorted = urls.clone();
        new_sorted.sort();
        let mut old_sorted = last.clone();
        old_sorted.sort();
        if new_sorted == old_sorted {
            return false; // unchanged — nothing to rebuild
        }
        let old_count = last.len();
        let new_count = urls.len();
        *last = urls;
        drop(last);
        tracing::info!(old_count, new_count, "relay map changed; node rebuild pending");
        *self.pending_rebuild.lock().expect("pending mutex poisoned") = Some(PendingRebuild {
            mode,
            since: Instant::now(),
            warned: false,
        });
        true
    }

    /// Execute a pending rebuild when the node is idle (or force it past
    /// [`RELAY_REBUILD_MAX_DEFER`]). No-op when nothing is pending. Clears the
    /// pending marker only on a successful rebuild — a failed re-bind stays pending
    /// so a later pass retries.
    async fn try_rebuild(self: &Arc<Self>) {
        let (mode, force) = {
            let mut pending = self.pending_rebuild.lock().expect("pending mutex poisoned");
            let Some(p) = pending.as_mut() else {
                return;
            };
            let deferred = p.since.elapsed();
            let force = deferred >= RELAY_REBUILD_MAX_DEFER;
            if force && !p.warned {
                p.warned = true;
                tracing::warn!(
                    duration_ms = deferred.as_millis() as u64,
                    "relay map rebuild deferred beyond max; forcing rebuild regardless of activity"
                );
            }
            (p.mode.clone(), force)
        };
        // Normally wait for a quiet instant; a force past MAX_DEFER bypasses the
        // idle gate so bounded staleness wins over an indefinitely-busy node.
        if !force && !self.is_idle() {
            return;
        }
        match self.rebuild(mode).await {
            Ok(()) => {
                *self.pending_rebuild.lock().expect("pending mutex poisoned") = None;
                // Log the completion (TEST-12): the pending marker used to clear
                // silently, so the only smoke signal was watching the node stop
                // re-homing. One `info!` turns the "did the rebuild land" check
                // into a one-line `query_logs`. `relay_count` = the freshly-applied
                // relay set the node is now homed on; `forced` = whether it fired
                // past the idle gate (max-defer) or on a quiet instant.
                let relay_count = self
                    .last_relay_urls
                    .lock()
                    .expect("last_relay_urls mutex poisoned")
                    .len();
                tracing::info!(relay_count, forced = force, "relay map node rebuild complete");
            }
            Err(e) => {
                tracing::error!(
                    error = %format!("{e:#}"),
                    "relay map node rebuild failed; will retry"
                );
            }
        }
    }

    /// Rebuild the endpoint + router on a new relay `mode` WITHOUT invalidating the
    /// `Arc<SharedIrohNode>` or any role handle (the riskiest piece of H2). Tears
    /// down the old endpoint/router/watcher (bounded), re-binds the SAME device
    /// secret (⇒ same node id) with the new relay mode against the SAME address
    /// lookup, re-mounts both protocols on the SAME store + demux + gate +
    /// responder, then swaps the rebuildable layer in under one lock. The store,
    /// peers, served map, key lock, and demux consumers are all preserved.
    async fn rebuild(self: &Arc<Self>, new_mode: RelayMode) -> Result<()> {
        // Tear the old endpoint down first (sequential, same secret ⇒ no relay-slot
        // overlap with the new bind), and abort the watcher. CRUCIALLY we do NOT
        // call `old_router.shutdown()`: the router's graceful shutdown invokes each
        // protocol handler's `shutdown()`, and `BlobsProtocol::shutdown()` tears
        // down the store actor — which is the SHARED store we must preserve. Closing
        // the endpoint ends the router's accept loop; the old `Router` clone is then
        // dropped at the swap below (drop aborts the accept task without touching the
        // store). The device-key lock stays held throughout (same identity).
        let (old_endpoint, old_watcher) = {
            let mut net = self.net.lock().expect("net mutex poisoned");
            (net.endpoint.clone(), net.relay_watcher.take())
        };
        if let Some(w) = old_watcher {
            w.abort();
        }
        if tokio::time::timeout(SHUTDOWN_CLOSE_TIMEOUT, old_endpoint.close())
            .await
            .is_err()
        {
            tracing::warn!("relay rebuild: old endpoint close timed out");
        }

        // Snapshot the new relay config before the builder consumes `new_mode`.
        let uses_relay = !matches!(new_mode, RelayMode::Disabled);
        let relay_map = new_mode.relay_map();
        let relay_urls: Vec<String> = relay_map
            .urls::<Vec<_>>()
            .iter()
            .map(|u| u.to_string())
            .collect();
        let secret_key = SecretKey::from_bytes(&self.device_secret);
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(secret_key)
            .relay_mode(new_mode)
            .address_lookup(self.lookup.clone())
            .bind()
            .await
            .context("re-bind iroh endpoint for relay rebuild")?;

        // Re-mount BOTH protocols on the SAME store, demux, gate, and responder —
        // so every Task-2 demux consumer stays registered and any installed
        // gate/responder survives, transparently to SyncRuntime/engines. Same
        // construction shape as bind() (`flush_store_on_shutdown: false` — the
        // shared store must survive this router's teardown). The resolver captures
        // the SAME (preserved) served map so the new consumer resolves identically.
        let serve_resolver: ServeRootResolver = {
            let served = Arc::clone(&self.served);
            Arc::new(move |hash| resolve_served_root_in(&served, hash))
        };
        let router = build_router(
            endpoint,
            &self.store,
            &self.connect_gate,
            EventSink::Demux(Arc::clone(&self.demux)),
            Arc::clone(&self.responder),
            false,
            serve_resolver,
        );
        let endpoint = router.endpoint().clone();
        let new_node_id: NodeId = *endpoint.id().as_bytes();
        debug_assert_eq!(
            new_node_id, self.node_id,
            "relay rebuild must preserve the node id (same device secret)"
        );
        let control_pool = ControlPool::new(endpoint.clone());
        // Respawn the watcher on the new endpoint, handing it the SAME wake-hook
        // and relay-health cells so a hook installed before the rebuild keeps
        // firing after it and the health surface tracks the new endpoint's relay.
        let relay_watcher = spawn_home_relay_watcher(
            endpoint.clone(),
            Arc::clone(&self.wake_hook),
            Arc::clone(&self.relay_health),
        );

        // Swap the freshly-built layer in — unless we raced host shutdown (which
        // set `shutdown_done` before taking the net lock), in which case discard
        // the new endpoint so it isn't leaked past teardown.
        {
            let mut net = self.net.lock().expect("net mutex poisoned");
            if self.shutdown_done.load(Ordering::SeqCst) {
                drop(net);
                // Discard the freshly-built layer without a graceful router
                // shutdown (that would tear down the SHARED store, which
                // `SharedIrohNode::shutdown` still owns): abort the watcher, close
                // the new endpoint, and let `router` drop here (drop aborts its
                // accept task, leaving the store intact).
                relay_watcher.abort();
                let _ = tokio::time::timeout(SHUTDOWN_CLOSE_TIMEOUT, endpoint.close()).await;
                drop(router);
                return Ok(());
            }
            net.endpoint = endpoint;
            net.router = router;
            net.control_pool = control_pool;
            net.relay_watcher = Some(relay_watcher);
            net.relay_urls = relay_urls.clone();
            net.uses_relay = uses_relay;
        }
        self.rebuild_count.fetch_add(1, Ordering::SeqCst);
        tracing::info!(
            node_id = %hex32(&self.node_id),
            relay_count = relay_urls.len(),
            "shared iroh node rebuilt after relay map change"
        );
        Ok(())
    }

    // ----- role-parameterized transport primitives (shared by every handle) ---

    /// Resolve a peer node id to a dialable target: the full addr from the peer
    /// book when known, else the bare id (resolved via address lookup).
    fn dial_target(&self, to: NodeId) -> Result<EndpointAddr> {
        if let Some(addr) = self.peers.lock().expect("peers mutex poisoned").get(&to) {
            return Ok(addr.clone());
        }
        let id = EndpointId::from_bytes(&to).map_err(|e| anyhow!("invalid peer node id: {e}"))?;
        Ok(EndpointAddr::new(id))
    }

    /// Short control-level reachability probe of `to` (Task 9), run by the download
    /// orchestrator before committing to a holder's long blob poll: a cheap
    /// `Endpoint::connect` on the control ALPN, bounded by `timeout`, classified
    /// into a [`ProbeClass`] on failure.
    ///
    /// `Ok(())` ⇒ the control connection established and stayed open past
    /// [`PROBE_CLOSE_WINDOW`] (reachable). `Err(class)`:
    /// - **Refused** — the connection established but the peer's [`ConnectGate`]
    ///   closed it (authz refusal), or `connect` errored with an application close.
    /// - **RelayUnreachable** — the dial timed out with a relay hint present.
    /// - **Offline** — no addressing info / no route (or a dial timeout with no
    ///   relay hint).
    ///
    /// A fresh connection is used (not the pooled one) so a refused/closing
    /// connection never pollutes the control pool; it is dropped (closed) on return,
    /// and the subsequent `request_project` dials its own pooled connection. This is
    /// a best-effort diagnostic — a reported class can misdirect a retry but never
    /// authenticate a holder (S5); content-hash + peer-authz remain the trust
    /// boundary.
    pub async fn probe_holder(
        &self,
        to: NodeId,
        has_relay_hint: bool,
        timeout: Duration,
    ) -> std::result::Result<(), ProbeClass> {
        let target = self.dial_target(to).map_err(|_| ProbeClass::Offline)?;
        // A target with neither a direct addr nor a relay is undialable — offline
        // without spending the timeout (deterministic; the download path always
        // attaches a relay dial hint first, so this only short-circuits a genuinely
        // address-less peer).
        if target.is_empty() {
            return Err(ProbeClass::Offline);
        }
        let endpoint = self.endpoint();
        match tokio::time::timeout(timeout, endpoint.connect(target, SYNC_ALPN)).await {
            Ok(Ok(conn)) => {
                // Handshake + ALPN succeeded. A refusing gate closes the fresh
                // connection almost immediately; a bounded wait separates refused
                // (peer closed) from reachable (stays open). The connection is
                // dropped (closed) at scope end either way.
                match tokio::time::timeout(PROBE_CLOSE_WINDOW, conn.closed()).await {
                    Ok(_reason) => Err(ProbeClass::Refused),
                    Err(_) => Ok(()),
                }
            }
            Ok(Err(e)) => Err(classify_connect_err(&format!("{e}"), has_relay_hint)),
            Err(_) => Err(if has_relay_hint {
                ProbeClass::RelayUnreachable
            } else {
                ProbeClass::Offline
            }),
        }
    }

    /// Open a bidi stream on the peer's pooled control connection, send one
    /// [`Msg`], and wait for the peer's application-level delivery ack. The
    /// connection is NOT closed after the send — it stays pooled for reuse (Task
    /// 2); the ack-before-return semantics are unchanged verbatim. Any error
    /// invalidates the pooled entry so the next send re-dials.
    async fn send_control(&self, to: NodeId, msg: Msg) -> Result<()> {
        let target = self.dial_target(to)?;
        let bytes = msg.encode()?;
        // Snapshot the current pooled-control handle so a concurrent relay rebuild
        // (which swaps in a fresh pool) never tears this send's connection out.
        let pool = self.control_pool();
        let conn = pool.get_or_connect(to, target).await?;

        let send = async {
            let (mut tx, mut rx) = conn.open_bi().await.context("open control stream")?;
            tx.write_all(&bytes).await.context("write control message")?;
            tx.finish().context("finish control stream")?;
            let ack = rx.read_to_end(8).await.context("await control delivery ack")?;
            if ack.is_empty() {
                anyhow::bail!("control message not acknowledged by peer");
            }
            anyhow::Ok(())
        };

        match tokio::time::timeout(CONTROL_SEND_TIMEOUT, send).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                pool.invalidate(to, &conn);
                Err(e)
            }
            Err(_) => {
                pool.invalidate(to, &conn);
                Err(anyhow!("sync control send to {} timed out", hex32(&to)))
            }
        }
    }

    /// Open a control connection to `to`, send one request [`Msg`], and read the
    /// peer's **reply `Msg`** off the same bidi stream (the request/response
    /// counterpart of [`send_control`](Self::send_control)). Used by the dedup
    /// handshake; each round drives its own connection/stream.
    async fn send_request(&self, to: NodeId, msg: Msg) -> Result<Msg> {
        let target = self.dial_target(to)?;
        let bytes = msg.encode()?;
        let pool = self.control_pool();
        let conn = pool.get_or_connect(to, target).await?;

        let exchange = async {
            let (mut tx, mut rx) = conn.open_bi().await.context("open control stream")?;
            tx.write_all(&bytes).await.context("write control request")?;
            tx.finish().context("finish control request")?;
            let reply = rx
                .read_to_end(MAX_CONTROL_BYTES)
                .await
                .context("await control reply")?;
            if reply.is_empty() {
                anyhow::bail!("control request closed without a reply");
            }
            let reply = Msg::decode(&reply).context("decode control reply")?;
            anyhow::Ok(reply)
        };

        match tokio::time::timeout(CONTROL_SEND_TIMEOUT, exchange).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(e)) => {
                pool.invalidate(to, &conn);
                Err(e)
            }
            Err(_) => {
                pool.invalidate(to, &conn);
                Err(anyhow!("sync control request to {} timed out", hex32(&to)))
            }
        }
    }

    async fn role_start(&self, role: Role) -> Result<StartInfo> {
        let prefix = role.prefix();
        // Prefix-scoped startup sweep (Д3), once per role. Every tag under this
        // role's `<prefix>/pkg/` namespace present at process start is stale
        // (package ids are per-process; a crash re-announces with fresh ids from
        // source dirs, and receiver fetch-tags never outlive an ack), so we
        // delete exactly them — never a sibling role's live tags on the shared
        // store, which the old `tags().delete_all()` would have wiped. GC-safe:
        // in-flight imports are protected by their own temp tags.
        if self.swept.lock().expect("swept mutex poisoned").insert(prefix) {
            let del_prefix = format!("{prefix}/pkg/");
            match self.store.tags().delete_prefix(del_prefix.as_bytes()).await {
                Ok(removed) if removed > 0 => tracing::info!(
                    role = prefix,
                    count = removed,
                    "blob store startup sweep removed stale tags"
                ),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(role = prefix, error = %e, "blob store startup sweep failed")
                }
            }
        }

        // Bounded home-relay wait, once for the whole node. With a relay
        // configured this lets the addr carry a relay url for NAT traversal;
        // with the relay disabled there is no home relay and `online()` would
        // hang, so we skip it entirely. Idempotent across roles.
        let endpoint = self.endpoint();
        if self.uses_relay() && !self.online_waited.swap(true, Ordering::SeqCst) {
            match tokio::time::timeout(ONLINE_TIMEOUT, endpoint.online()).await {
                Ok(()) => {
                    let relay_url = endpoint
                        .addr()
                        .relay_urls()
                        .next()
                        .map(|u| u.to_string());
                    // Seed the health cell for the status surface (Task 3.3): the
                    // relay is connected right now, so `transport_health` can report
                    // `relay_connected` immediately, before the watcher records its
                    // first transition. This is a BRIDGE only — the watcher then
                    // overwrites this cell on every later transition (including a
                    // disconnect), so it never masks a dropped relay.
                    *self.relay_health.write().expect("relay_health lock poisoned") = RelayHealth {
                        connected: true,
                        url: relay_url.clone(),
                        last_error: None,
                        since: Instant::now(),
                    };
                    tracing::info!(
                        node_id = %endpoint.id().fmt_short(),
                        relay_url = relay_url.as_deref().unwrap_or("unknown"),
                        "home relay connected"
                    );
                }
                Err(_) => {
                    // The wait ran and timed out: leave the health cell to the
                    // watcher (its initial baseline is disconnected), so we are
                    // direct-only until the watcher sees a connection.
                    tracing::warn!(
                        node_id = %endpoint.id().fmt_short(),
                        timeout_ms = ONLINE_TIMEOUT.as_millis() as u64,
                        "home relay wait timed out; proceeding on direct addresses only (unreachable behind NAT)"
                    );
                }
            }
        }

        let addr = endpoint.addr();
        let pairing_ticket = EndpointTicket::from(addr).to_string();
        tracing::debug!(role = prefix, node_id = %endpoint.id().fmt_short(), "shared iroh node role started");
        Ok(StartInfo {
            node_id: self.node_id,
            pairing_ticket,
        })
    }

    async fn role_announce(&self, role: Role, to: NodeId, a: &PackageAnnounce) -> Result<()> {
        let _op = self.op_guard();
        let tag = role_package_tag(role.prefix(), &a.package_id);
        let mut wire = a.clone();
        {
            let served = self.served.lock().expect("served mutex poisoned");
            match served.get(&tag) {
                Some(hash) => wire.root_hash = hash.to_string(),
                None => tracing::warn!(
                    package_id = %a.package_id.0,
                    "announce without a served collection; forwarding placeholder root_hash"
                ),
            }
        }
        self.send_control(to, Msg::Announce(wire)).await?;
        tracing::debug!(to = %hex32(&to), package_id = %a.package_id.0, "iroh announce sent");
        Ok(())
    }

    async fn role_announce_project(
        &self,
        role: Role,
        to: NodeId,
        project_id: &str,
        package_id: &str,
        a: &PackageAnnounce,
    ) -> Result<()> {
        let _op = self.op_guard();
        let tag = role_package_tag(role.prefix(), &a.package_id);
        let mut wire = a.clone();
        {
            let served = self.served.lock().expect("served mutex poisoned");
            match served.get(&tag) {
                Some(hash) => wire.root_hash = hash.to_string(),
                None => tracing::warn!(
                    package_id = %a.package_id.0,
                    "project announce without a served collection; forwarding placeholder root_hash"
                ),
            }
        }
        self.send_control(
            to,
            Msg::ProjectAnnounce {
                project_id: project_id.to_string(),
                package_id: package_id.to_string(),
                announce: wire,
            },
        )
        .await?;
        tracing::debug!(
            to = %hex32(&to),
            project_id,
            package_id,
            wire_package_id = %a.package_id.0,
            "iroh project announce sent"
        );
        Ok(())
    }

    async fn role_request_project(
        &self,
        to: NodeId,
        project_id: &str,
        package_id: &str,
    ) -> Result<()> {
        let _op = self.op_guard();
        self.send_control(
            to,
            Msg::ProjectRequest {
                project_id: project_id.to_string(),
                package_id: package_id.to_string(),
            },
        )
        .await?;
        tracing::debug!(to = %hex32(&to), project_id, package_id, "iroh project request sent");
        Ok(())
    }

    async fn role_fetch(
        &self,
        role: Role,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
        sink: FetchSink,
    ) -> Result<()> {
        let _op = self.op_guard();
        let root_hash: Hash = pkg.root_hash.parse().with_context(|| {
            format!("parse collection hash from announce root_hash {:?}", pkg.root_hash)
        })?;
        let provider =
            EndpointId::from_bytes(&from).map_err(|e| anyhow!("invalid provider node id: {e}"))?;
        let tag = role_package_tag(role.prefix(), &pkg.package_id);
        // Snapshot the endpoint so a concurrent relay rebuild can't swap it out
        // mid-download.
        let endpoint = self.endpoint();
        blobs::fetch_collection_to_dir(
            &self.store,
            &endpoint,
            provider,
            root_hash,
            &tag,
            dest_dir,
            pkg.byte_size,
            sink,
        )
        .await?;

        tracing::debug!(from = %hex32(&from), package_id = %pkg.package_id.0, "iroh fetch complete");
        Ok(())
    }

    async fn role_fetch_manifest(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
    ) -> Result<PathBuf> {
        let _op = self.op_guard();
        let root_hash: Hash = pkg.root_hash.parse().with_context(|| {
            format!("parse collection hash from announce root_hash {:?}", pkg.root_hash)
        })?;
        let provider =
            EndpointId::from_bytes(&from).map_err(|e| anyhow!("invalid provider node id: {e}"))?;
        // Snapshot the endpoint so a concurrent relay rebuild can't swap it out.
        let endpoint = self.endpoint();
        blobs::fetch_manifest_to_dir(&self.store, &endpoint, provider, root_hash, dest_dir).await
    }

    async fn role_serve(
        &self,
        role: Role,
        pkg: &PackageAnnounce,
        src_dir: &Path,
        want: Option<&HashSet<String>>,
    ) -> Result<()> {
        let _op = self.op_guard();
        let tag = role_package_tag(role.prefix(), &pkg.package_id);
        let hash = match want {
            None => blobs::import_package_collection(&self.store, src_dir, &tag).await?,
            Some(w) => blobs::import_subset_collection(&self.store, src_dir, w, &tag).await?,
        };
        self.served
            .lock()
            .expect("served mutex poisoned")
            .insert(tag, hash);
        tracing::debug!(
            package_id = %pkg.package_id.0,
            root_hash = %hash,
            path = %src_dir.display(),
            subset = want.is_some(),
            "iroh serving package"
        );
        Ok(())
    }

    async fn role_release(&self, role: Role, package_id: &PackageId) -> Result<()> {
        let tag = role_package_tag(role.prefix(), package_id);
        self.served.lock().expect("served mutex poisoned").remove(&tag);
        let removed = self
            .store
            .tags()
            .delete(tag.as_bytes())
            .await
            .map_err(|e| anyhow!("delete package tag: {e}"))?;
        tracing::debug!(package_id = %package_id.0, tags_removed = removed, "iroh released package");
        Ok(())
    }

    async fn ack_inner(
        &self,
        to: NodeId,
        package_id: &PackageId,
        receipts: Vec<FrameReceipt>,
    ) -> Result<()> {
        let _op = self.op_guard();
        let count = receipts.len();
        self.send_control(
            to,
            Msg::Ack {
                package_id: package_id.clone(),
                receipts,
            },
        )
        .await?;
        tracing::debug!(to = %hex32(&to), package_id = %package_id.0, count, "iroh ack sent");
        Ok(())
    }

    async fn negotiate_want_inner(
        &self,
        to: NodeId,
        package_id: PackageId,
        offer: Vec<OfferEntry>,
        full_by_rel: HashMap<String, String>,
    ) -> Result<HashSet<String>> {
        let _op = self.op_guard();
        let reply = self
            .send_request(
                to,
                Msg::Offer {
                    package_id: package_id.clone(),
                    entries: offer.clone(),
                },
            )
            .await
            .context("negotiate_want offer round")?;
        let (want, candidates) = match reply {
            Msg::Want { want, candidates, .. } => (want, candidates),
            other => anyhow::bail!("expected Want reply to Offer, got {other:?}"),
        };

        let mut wanted: HashSet<String> = want.into_iter().collect();
        if candidates.is_empty() {
            tracing::debug!(to = %hex32(&to), package_id = %package_id.0, want = wanted.len(), "negotiate_want resolved (no candidates)");
            return Ok(wanted);
        }

        let entries = proto::build_full_hash_entries(&offer, &candidates, &full_by_rel, &mut wanted);
        if !entries.is_empty() {
            let reply = self
                .send_request(
                    to,
                    Msg::FullHashes {
                        package_id: package_id.clone(),
                        entries,
                    },
                )
                .await
                .context("negotiate_want full-hashes round")?;
            let still = match reply {
                Msg::Want { want, .. } => want,
                other => anyhow::bail!("expected Want reply to FullHashes, got {other:?}"),
            };
            wanted.extend(still);
        }
        tracing::debug!(to = %hex32(&to), package_id = %package_id.0, want = wanted.len(), "negotiate_want resolved");
        Ok(wanted)
    }
}

/// A per-[`Role`] view onto a [`SharedIrohNode`], implementing
/// [`SharingTransport`] with role-prefixed blob tags (Д3). Cheap to clone
/// (an `Arc` + a `Copy` role). The node it references is torn down by the host
/// via [`SharedIrohNode::shutdown`], not by any one handle.
pub struct RoleHandle {
    node: Arc<SharedIrohNode>,
    role: Role,
    /// Unique id so the demux can attribute this handle's ack claims + Recv
    /// consumer registration and release them all when the handle drops.
    id: u64,
    /// This handle's own events channel sender, set on the first
    /// [`events`](SharingTransport::events) call; an ack claim points at it.
    events_tx: Mutex<Option<mpsc::Sender<TransportEvent>>>,
}

impl RoleHandle {
    /// The shared node's blob store (used by tests to prove two role handles read
    /// the SAME store instance).
    #[allow(dead_code)] // consumed by the shared-store test below
    pub(crate) fn store(&self) -> &Store {
        self.node.store()
    }

    /// Register an ack claim for `(to, package_id)` pointing at this handle's
    /// events channel, so the eventual [`AckReceived`](TransportEvent::AckReceived)
    /// routes back here (Task 2). A no-op-with-warn if
    /// [`events`](SharingTransport::events) has not been taken yet (the ack would
    /// then have no claimant) — the engine always takes it first.
    fn claim_ack(&self, to: NodeId, package_id: &PackageId) {
        let tx = self.events_tx.lock().expect("events_tx mutex poisoned").clone();
        match tx {
            Some(tx) => self
                .node
                .demux
                .register_claim(self.id, (to, package_id.clone()), tx),
            None => tracing::warn!(
                package_id = %package_id.0,
                to = %hex32(&to),
                "announce before events() taken; ack will have no claimant"
            ),
        }
    }
}

impl Drop for RoleHandle {
    fn drop(&mut self) {
        // Release every claim + the Recv registration this handle owned, so
        // nothing routes to a dropped handle's dead channel (claims can't leak).
        self.node.demux.release_handle(self.id);
    }
}

#[async_trait]
impl SharingTransport for RoleHandle {
    async fn start(&self) -> Result<StartInfo> {
        self.node.role_start(self.role).await
    }

    async fn announce(&self, to: NodeId, a: &PackageAnnounce) -> Result<()> {
        // Claim the eventual ack for THIS handle before announcing; release the
        // claim if the announce itself fails (claims can't leak on the error path).
        self.claim_ack(to, &a.package_id);
        let res = self.node.role_announce(self.role, to, a).await;
        if res.is_err() {
            self.node.demux.release_claim(&(to, a.package_id.clone()));
        }
        res
    }

    async fn fetch(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
        sink: FetchSink,
    ) -> Result<()> {
        self.node
            .role_fetch(self.role, from, pkg, dest_dir, sink)
            .await
    }

    async fn fetch_manifest(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
    ) -> Result<PathBuf> {
        self.node.role_fetch_manifest(from, pkg, dest_dir).await
    }

    async fn serve(
        &self,
        pkg: &PackageAnnounce,
        src_dir: &Path,
        want: Option<&HashSet<String>>,
    ) -> Result<()> {
        self.node.role_serve(self.role, pkg, src_dir, want).await
    }

    async fn ack(
        &self,
        to: NodeId,
        package_id: &PackageId,
        receipts: Vec<FrameReceipt>,
    ) -> Result<()> {
        self.node.ack_inner(to, package_id, receipts).await
    }

    async fn negotiate_want(
        &self,
        to: NodeId,
        package_id: PackageId,
        offer: Vec<OfferEntry>,
        full_by_rel: HashMap<String, String>,
    ) -> Result<HashSet<String>> {
        self.node
            .negotiate_want_inner(to, package_id, offer, full_by_rel)
            .await
    }

    async fn announce_project(
        &self,
        to: NodeId,
        project_id: &str,
        package_id: &str,
        announce: &PackageAnnounce,
    ) -> Result<()> {
        // The wire announce's package_id is the ack-correlation id — claim on it.
        self.claim_ack(to, &announce.package_id);
        let res = self
            .node
            .role_announce_project(self.role, to, project_id, package_id, announce)
            .await;
        if res.is_err() {
            self.node.demux.release_claim(&(to, announce.package_id.clone()));
        }
        res
    }

    async fn request_project(
        &self,
        to: NodeId,
        project_id: &str,
        package_id: &str,
    ) -> Result<()> {
        self.node.role_request_project(to, project_id, package_id).await
    }

    async fn release(&self, package_id: &PackageId) -> Result<()> {
        self.node.role_release(self.role, package_id).await
    }

    fn add_peer_dial_hint(&self, from: NodeId) {
        // I2 (T7): route the blob-pull dial hint to the shared node's address
        // lookup (relay-only, our own relay set). The loopback transport keeps the
        // trait default no-op.
        self.node.add_peer_dial_hint(from);
    }

    fn add_peer_addr(&self, addr: EndpointAddr) {
        // T8 retry re-resolution: register the peer's refreshed address on the
        // shared node (peer book + address lookup), so the engine's next re-attempt
        // dials the peer's current relay/direct path.
        self.node.add_peer(addr);
    }

    async fn events(&self) -> mpsc::Receiver<TransportEvent> {
        // Task 2 demux: each handle gets its OWN channel. The Recv handle
        // registers as the single inbound consumer (announces/requests); an
        // Out/Collab handle stores its sender so its announces can claim the
        // matching ack. The engine takes this exactly once per handle.
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        *self.events_tx.lock().expect("events_tx mutex poisoned") = Some(tx.clone());
        if self.role == Role::Recv {
            self.node.demux.register_recv(self.id, tx);
        }
        rx
    }

    async fn shutdown(&self) {
        // A role handle is a view onto the shared node; the host owns the node's
        // lifecycle and calls `SharedIrohNode::shutdown` directly (Task 3 wiring).
        // Tearing the whole node down from one role handle would kill its
        // siblings, so this is intentionally a no-op.
    }
}

/// Spawn the home-relay status watcher (spec §1, minor #4). iroh's
/// [`home_relay_status`](iroh::Endpoint::home_relay_status) watcher surfaces relay
/// connectivity transitions — a relay eviction (`SameEndpointIdConnected`, the C1
/// symptom) shows up here as a disconnect instead of generic downstream timeouts.
/// We log only *transitions* (`info!` on connect, `warn!` on disconnect) so a
/// steady state is silent. The task ends when the last endpoint clone drops; the
/// node aborts it at shutdown (closing the endpoint alone does not stop it).
///
/// A relay **connect** transition is a wake event (Task 6): the node just (re)came
/// online, so it fires the installed [`WakeHook`] to kick every pending outbound
/// package out of its backoff. `wake_hook` is the node's shared hook lock — read
/// (and cloned out from under) at FIRE time, never captured here at spawn — so a
/// hook installed after this task spawned, or re-installed across a relay rebuild
/// that respawns this watcher, is always the one that fires.
fn spawn_home_relay_watcher(
    endpoint: Endpoint,
    wake_hook: Arc<RwLock<Option<WakeHook>>>,
    relay_health: Arc<RwLock<RelayHealth>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        use n0_future::StreamExt as _;
        let mut stream = endpoint.home_relay_status().stream();
        let mut last: HashMap<String, bool> = HashMap::new();
        while let Some(statuses) = stream.next().await {
            for st in &statuses {
                let url = st.url().to_string();
                let connected = st.is_connected();
                if last.get(&url).copied() == Some(connected) {
                    continue;
                }
                // Record the transition for the queryable health surface (Task
                // 3.3) — the same values these log lines carry. Written under the
                // lock, no await held; a poll reads the current picture.
                let last_error = if connected { None } else { st.last_error().map(|e| e.to_string()) };
                *relay_health.write().expect("relay_health lock poisoned") = RelayHealth {
                    connected,
                    url: Some(url.clone()),
                    last_error: last_error.clone(),
                    since: Instant::now(),
                };
                if connected {
                    tracing::info!(relay_url = %url, "home relay connected");
                    // Wake event: clone the hook out from under the lock, then
                    // invoke it with no lock held (no reentrant deadlock).
                    let hook = wake_hook.read().expect("wake_hook lock poisoned").clone();
                    if let Some(h) = hook {
                        h();
                    }
                } else if let Some(err) = last_error.as_deref() {
                    tracing::warn!(relay_url = %url, error = %err, "home relay disconnected");
                } else {
                    tracing::warn!(relay_url = %url, "home relay disconnected");
                }
                last.insert(url, connected);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    //! Loopback tests for the four T1 invariants (all relay-disabled, no
    //! networking): the device-key lock, the prefix-scoped sweep, clean
    //! bind→shutdown→re-bind, and one store shared by two role handles.

    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use super::*;
    use crate::package::{write_package, ManifestRecord, PayloadKind, MANIFEST_VERSION};

    async fn tag_present(store: &Store, name: &str) -> bool {
        store
            .tags()
            .get(name.as_bytes())
            .await
            .expect("tags().get")
            .is_some()
    }

    /// A minimal one-frame package (payload + manifest) for the shared-store
    /// import test. `announce.root_hash` is the xxh3 placeholder; the transport
    /// swaps in the iroh collection hash at serve time.
    fn build_one_frame_package(base: &Path) -> (PathBuf, PackageAnnounce) {
        let src = base.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let payload = src.join("frame.fits");
        std::fs::write(&payload, b"hello frame payload for the shared store test").unwrap();
        let byte_size = std::fs::metadata(&payload).unwrap().len();
        let xxh3 = crate::package::xxh3_full_file(&payload).unwrap();
        let record = ManifestRecord {
            v: MANIFEST_VERSION,
            frame_uuid: "uuid-d".to_string(),
            origin_catalog_uuid: "catalog-uuid".to_string(),
            origin_device: "origin-device".to_string(),
            payload_kind: PayloadKind::RawFrame,
            rel_path: "frame.fits".to_string(),
            byte_size,
            xxh3,
            frame_meta: serde_json::json!({ "object": "M42" }),
            analysis: None,
            app_version: "test".to_string(),
            project: None,
        };
        let pkg_dir = base.join("pkg");
        let announce = write_package(&pkg_dir, vec![(payload, record)]).unwrap();
        (pkg_dir, announce)
    }

    /// A two-frame package with DELIBERATELY distinct payload sizes (Task 13
    /// regression test): a real iroh collection for even one frame already has
    /// multiple blobs (the hash-seq blob + the manifest + the frame payload), so
    /// two frames of different sizes give a clear, hard-to-fake signal that
    /// upload-progress reporting is truly cumulative across ALL of them — a
    /// per-blob-only bug would freeze at whichever single blob is largest
    /// (`size_b`, assuming `size_b > size_a` and both dwarf the hash-seq/manifest
    /// blobs), never reaching the collection's full announced `byte_size`.
    fn build_two_frame_package(base: &Path, size_a: usize, size_b: usize) -> (PathBuf, PackageAnnounce) {
        let src = base.join("src2");
        std::fs::create_dir_all(&src).unwrap();
        let mut records = Vec::new();
        for (i, (name, size)) in [("frame_a.fits", size_a), ("frame_b.fits", size_b)]
            .into_iter()
            .enumerate()
        {
            let payload = src.join(name);
            // Distinct, non-repeating content per file so a wrong-blob mixup would
            // also fail a hash check, not just a size check.
            let bytes: Vec<u8> = (0..size).map(|j| ((j + i * 97) % 251) as u8).collect();
            std::fs::write(&payload, &bytes).unwrap();
            let byte_size = std::fs::metadata(&payload).unwrap().len();
            let xxh3 = crate::package::xxh3_full_file(&payload).unwrap();
            records.push((
                payload,
                ManifestRecord {
                    v: MANIFEST_VERSION,
                    frame_uuid: format!("uuid-two-{i}"),
                    origin_catalog_uuid: "catalog-uuid".to_string(),
                    origin_device: "origin-device".to_string(),
                    payload_kind: PayloadKind::RawFrame,
                    rel_path: name.to_string(),
                    byte_size,
                    xxh3,
                    frame_meta: serde_json::json!({ "object": "M42" }),
                    analysis: None,
                    app_version: "test".to_string(),
                    project: None,
                },
            ));
        }
        let pkg_dir = base.join("pkg2");
        let announce = write_package(&pkg_dir, records).unwrap();
        (pkg_dir, announce)
    }

    // (a) Two binds on the same sync_dir: the second fails on the device-key
    //     advisory lock with the actionable, key-material-free message.
    #[tokio::test]
    async fn second_bind_same_dir_fails_with_lock_message() {
        let dir = tempdir().unwrap();
        let node1 = SharedIrohNode::bind(dir.path(), RelayMode::Disabled)
            .await
            .expect("first bind succeeds");

        // A second bind over the same device key must fail. (`Arc<SharedIrohNode>`
        // isn't `Debug`, so match rather than `expect_err`.)
        let err = match SharedIrohNode::bind(dir.path(), RelayMode::Disabled).await {
            Ok(_) => panic!("second bind on the same device key must fail"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("in use by another process"),
            "lock error must name the contention, got: {msg}"
        );
        assert!(
            msg.contains("each install needs its own identity"),
            "lock error must be actionable, got: {msg}"
        );

        node1.shutdown().await;
    }

    // (b) The startup sweep is prefix-scoped: starting the Out handle deletes
    //     only `out/pkg/*`, never a sibling role's live tags on the shared store.
    #[tokio::test]
    async fn startup_sweep_scoped_to_role_prefix() {
        let dir = tempdir().unwrap();
        let node = SharedIrohNode::bind(dir.path(), RelayMode::Disabled)
            .await
            .unwrap();

        // Seed one blob and tag it across all three role prefixes.
        let tt = node
            .store()
            .blobs()
            .add_bytes(b"seed".to_vec())
            .temp_tag()
            .await
            .unwrap();
        for name in ["out/pkg/a", "recv/pkg/b", "collab/pkg/c"] {
            node.store()
                .tags()
                .set(name, tt.hash_and_format())
                .await
                .unwrap();
        }
        drop(tt);

        // Start ONLY the Out handle → sweeps only the `out/pkg/` prefix.
        let out = node.handle(Role::Out);
        out.start().await.unwrap();

        assert!(
            !tag_present(node.store(), "out/pkg/a").await,
            "the Out startup sweep must delete out/pkg/a"
        );
        assert!(
            tag_present(node.store(), "recv/pkg/b").await,
            "a foreign role's tag (recv/pkg/b) must survive the Out sweep"
        );
        assert!(
            tag_present(node.store(), "collab/pkg/c").await,
            "a foreign role's tag (collab/pkg/c) must survive the Out sweep"
        );

        node.shutdown().await;
    }

    // (c) bind → shutdown → re-bind on the same dir succeeds: the lock is
    //     released and the store closed cleanly, and the identity is stable.
    #[tokio::test]
    async fn bind_shutdown_rebind_same_dir_succeeds() {
        let dir = tempdir().unwrap();
        let node1 = SharedIrohNode::bind(dir.path(), RelayMode::Disabled)
            .await
            .expect("first bind");
        let id1 = node1.node_id();
        node1.shutdown().await;

        let node2 = SharedIrohNode::bind(dir.path(), RelayMode::Disabled)
            .await
            .expect("re-bind after shutdown must succeed (lock released, store closed)");
        assert_eq!(
            node2.node_id(),
            id1,
            "the same device key must yield the same node id across a re-bind"
        );
        node2.shutdown().await;
    }

    // Task 3.3: a relay-disabled node has no home relay, so its transport health
    // is `direct_only` (undialable behind NAT) — never `relay_connected`. This is
    // the node-level half of the health derivation (the api layer owns
    // `not_started` / `no_relay_map`). No role is started and no relay is
    // configured, so the online-wait never runs and the watcher records nothing —
    // the bind-time `direct_only` baseline stands.
    #[tokio::test]
    async fn transport_health_relay_disabled_is_direct_only() {
        let dir = tempdir().unwrap();
        let node = bind_disabled(dir.path()).await;
        let health = node.transport_health();
        assert_eq!(health.status, "direct_only", "relay-disabled node is direct-only");
        assert!(health.relay_url.is_none(), "no relay configured => no relay url");
        node.shutdown().await;
    }

    // Task 3.3 regression: a relay-configured node that reports connected (the
    // online-wait seed / the watcher's first connect transition) must flip to
    // `direct_only` the moment the watcher records a DISCONNECT — the home relay
    // dropped. This is the exact path the old `online_ok` one-shot latch masked:
    // `health.connected || online_ok` stayed true forever after the single
    // successful `online()` wait, so a later relay drop still read
    // `relay_connected`. With `online_ok` gone, the `RelayHealth` cell is the sole
    // authority, so the disconnect transition wins.
    #[tokio::test]
    async fn transport_health_relay_drop_flips_to_direct_only() {
        let dir = tempdir().unwrap();
        let node = bind_disabled(dir.path()).await;
        // Loopback binds are relay-disabled; force the relay-configured branch so
        // the derivation depends on the health cell (not the disabled short-circuit).
        node.force_uses_relay_for_test(true);

        // 1) Connected: seeded by the successful online-wait / watcher's first
        //    connect transition.
        node.set_relay_health_for_test(RelayHealth {
            connected: true,
            url: Some("https://relay.example".to_string()),
            last_error: None,
            since: Instant::now(),
        });
        assert_eq!(
            node.transport_health().status,
            "relay_connected",
            "a connected relay reads relay_connected"
        );

        // 2) The watcher then records a DISCONNECT (home relay dropped).
        node.set_relay_health_for_test(RelayHealth {
            connected: false,
            url: Some("https://relay.example".to_string()),
            last_error: Some("relay closed".to_string()),
            since: Instant::now(),
        });
        let health = node.transport_health();
        assert_eq!(
            health.status, "direct_only",
            "a dropped relay must flip the surface to direct_only (regression: the \
             online_ok latch reported relay_connected forever)"
        );
        assert_eq!(
            health.last_error.as_deref(),
            Some("relay closed"),
            "the disconnect's last_error is surfaced"
        );

        node.shutdown().await;
    }

    // (d) Two role handles share ONE store: import a package via the Out handle,
    //     observe its tag through the Recv handle's store (the same instance).
    #[tokio::test]
    async fn two_role_handles_share_one_store() {
        let dir = tempdir().unwrap();
        let node = SharedIrohNode::bind(dir.path(), RelayMode::Disabled)
            .await
            .unwrap();

        let out = node.role_handle(Role::Out);
        let recv = node.role_handle(Role::Recv);
        // Structural proof: both handles reference the very same store instance.
        assert!(
            std::ptr::eq(out.store(), recv.store()),
            "Out and Recv handles must reference one shared store"
        );

        // Functional proof: import via Out, read the resulting tag via Recv.
        let (pkg_dir, announce) = build_one_frame_package(dir.path());
        out.serve(&announce, &pkg_dir, None).await.unwrap();

        let tag = format!("out/pkg/{}", announce.package_id.0);
        assert!(
            tag_present(recv.store(), &tag).await,
            "a package imported via the Out handle must be visible through the Recv handle's store"
        );

        node.shutdown().await;
    }

    // Task 13: a served collection's root hash resolves back to its package id
    // (the reverse lookup the provider-upload-events consumer labels progress
    // with); an unknown hash resolves to None.
    #[tokio::test]
    async fn resolve_served_root_maps_collection_hash_to_package_id() {
        let dir = tempdir().unwrap();
        let node = SharedIrohNode::bind(dir.path(), RelayMode::Disabled)
            .await
            .unwrap();

        let out = node.role_handle(Role::Out);
        let (pkg_dir, announce) = build_one_frame_package(dir.path());
        out.serve(&announce, &pkg_dir, None).await.unwrap();

        // The collection root hash the serve registered, read back off its tag.
        let tag = format!("out/pkg/{}", announce.package_id.0);
        let root = node
            .store()
            .tags()
            .get(tag.as_bytes())
            .await
            .unwrap()
            .expect("served tag present")
            .hash;

        assert_eq!(
            node.resolve_served_root(root),
            Some(announce.package_id.clone()),
            "the served collection root must resolve back to its package id"
        );
        assert_eq!(
            node.resolve_served_root(Hash::new(b"an unserved blob")),
            None,
            "an unknown hash must resolve to None (child blob / foreign / hash-seq internal)"
        );

        node.shutdown().await;
    }

    // Task 13 fix (reviewer-caught regression): over a REAL iroh transfer, a
    // multi-blob collection's cumulative upload progress must reach the FULL
    // announced byte_size, not freeze at the single largest blob's size. Two
    // real `SharedIrohNode`s, relay disabled (localhost direct addresses, no
    // mock) — this exercises the actual `iroh_blobs::provider::events` stream
    // and the `UploadAccumulator` wired into `build_router`'s consumer, pinning
    // the fix against the real provider API (not just the pure accumulator unit
    // tests above).
    #[tokio::test]
    async fn serve_progress_over_real_iroh_accumulates_across_collection_blobs() {
        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r = bind_disabled(dr.path()).await;

        let out = s.handle(Role::Out);
        let recv = r.handle(Role::Recv);
        let s_info = out.start().await.unwrap();
        let r_info = recv.start().await.unwrap();
        pair(&s, &s_info, &r, &r_info);

        // Two frames of clearly different sizes (4 KiB, 64 KiB) so a per-blob
        // (not cumulative) bug would visibly undershoot the full byte_size.
        let (pkg_dir, announce) = build_two_frame_package(ds.path(), 4 * 1024, 64 * 1024);
        out.serve(&announce, &pkg_dir, None).await.unwrap();

        // Register both handles' consumers BEFORE announcing (same ordering the
        // other demux tests use): `out.events()` registers the ack-claim channel
        // that `route_serve_progress` will target once `announce` claims it.
        let mut out_events = out.events().await;
        let mut r_ev = recv.events().await;

        out.announce(r_info.node_id, &announce).await.unwrap();
        let wire = match recv_next(&mut r_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce,
            other => panic!("expected AnnounceReceived, got {other:?}"),
        };

        // A real blob download over QUIC: this is what drives the provider's
        // `GetRequestReceivedNotify` → our consumer → `ServeProgress` ticks.
        let dest = tempdir().unwrap();
        recv.fetch(s_info.node_id, &wire, dest.path(), crate::sharing::noop_fetch_sink())
            .await
            .unwrap();

        // Poll for ServeProgress ticks until the peak reaches the full collection
        // size (the consumer's close-time flush may land slightly after `fetch`
        // returns — it runs on a detached task independent of the receiver's
        // completion) or the bound elapses.
        let mut peak = 0u64;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while peak < announce.byte_size && std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match tokio::time::timeout(remaining, out_events.recv()).await {
                Ok(Some(TransportEvent::ServeProgress { bytes_sent, .. })) => {
                    peak = peak.max(bytes_sent);
                }
                // The terminal `ServeComplete` (Task 2.1) rides the same channel
                // after the final progress flush — skip it, don't treat it as an
                // early break, so a timing quirk can't truncate the poll.
                Ok(Some(TransportEvent::ServeComplete { .. })) => continue,
                Ok(Some(_)) | Ok(None) => break,
                Err(_) => break, // overall deadline elapsed
            }
        }

        assert!(
            peak >= announce.byte_size,
            "cumulative upload progress must reach the full collection size ({}), got peak={peak} \
             (a per-blob-only regression would freeze at the largest single frame's size, ~64KiB)",
            announce.byte_size
        );

        s.shutdown().await;
        r.shutdown().await;
    }

    /// Collect every [`TransportEvent`] arriving on `rx` until it goes quiet for
    /// `quiet` (no new event within that window) or the overall `max` elapses.
    /// A quiet-window drain lets a NEGATIVE assertion (nothing arrives) return
    /// promptly while still bounding a POSITIVE one.
    async fn collect_until_quiet(
        rx: &mut Receiver<TransportEvent>,
        quiet: Duration,
        max: Duration,
    ) -> Vec<TransportEvent> {
        let mut out = Vec::new();
        let deadline = tokio::time::Instant::now() + max;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            let wait = quiet.min(deadline - now);
            match tokio::time::timeout(wait, rx.recv()).await {
                Ok(Some(ev)) => out.push(ev),
                Ok(None) => break, // channel closed
                Err(_) => break,   // quiet window elapsed with no new event
            }
        }
        out
    }

    // Task 2.1: a payload-carrying full pull emits EXACTLY ONE `ServeComplete`
    // (the terminal `Completed` of the phase-2 payload request), while a
    // `fetch_manifest` probe emits NEITHER a `ServeComplete` NOR any
    // `ServeProgress` tick — its only requests are the phase-1 root+meta pull
    // (resolves to the package but is NOT payload-carrying) and the manifest's
    // own raw blob (a different root the resolver maps to None). The full pull at
    // the end proves the provider-event plumbing is live, so the probe's silence
    // is a meaningful assertion, not a vacuous pass. Two real nodes, relay off.
    #[tokio::test]
    async fn manifest_probe_is_silent_and_full_pull_fires_one_serve_complete() {
        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r = bind_disabled(dr.path()).await;

        let out = s.handle(Role::Out);
        let recv = r.handle(Role::Recv);
        let s_info = out.start().await.unwrap();
        let r_info = recv.start().await.unwrap();
        pair(&s, &s_info, &r, &r_info);

        let (pkg_dir, announce) = build_two_frame_package(ds.path(), 4 * 1024, 64 * 1024);
        out.serve(&announce, &pkg_dir, None).await.unwrap();

        // Register consumers BEFORE announcing so the ack-claim channel exists for
        // route_serve_progress / route_serve_complete to target.
        let mut out_events = out.events().await;
        let mut r_ev = recv.events().await;
        out.announce(r_info.node_id, &announce).await.unwrap();
        let wire = match recv_next(&mut r_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce,
            other => panic!("expected AnnounceReceived, got {other:?}"),
        };

        // (a) Manifest probe: NO ServeProgress, NO ServeComplete.
        let mdest = tempdir().unwrap();
        recv.fetch_manifest(s_info.node_id, &wire, mdest.path())
            .await
            .unwrap();
        let probe = collect_until_quiet(
            &mut out_events,
            Duration::from_millis(1500),
            Duration::from_secs(4),
        )
        .await;
        assert!(
            !probe
                .iter()
                .any(|e| matches!(e, TransportEvent::ServeComplete { .. })),
            "a manifest probe must not emit ServeComplete, got {probe:?}"
        );
        assert!(
            !probe
                .iter()
                .any(|e| matches!(e, TransportEvent::ServeProgress { .. })),
            "a manifest probe must not emit a ServeProgress tick, got {probe:?}"
        );

        // (b) Full pull of the payload: EXACTLY ONE ServeComplete.
        let fdest = tempdir().unwrap();
        recv.fetch(s_info.node_id, &wire, fdest.path(), crate::sharing::noop_fetch_sink())
            .await
            .unwrap();
        let pull = collect_until_quiet(
            &mut out_events,
            Duration::from_millis(1500),
            Duration::from_secs(10),
        )
        .await;
        let completes = pull
            .iter()
            .filter(|e| matches!(e, TransportEvent::ServeComplete { .. }))
            .count();
        assert_eq!(
            completes, 1,
            "a full payload pull must emit exactly one ServeComplete, got {completes} in {pull:?}"
        );
        assert!(
            pull.iter().any(|e| matches!(
                e,
                TransportEvent::ServeComplete { package_id } if *package_id == announce.package_id
            )),
            "ServeComplete must carry the served package id"
        );

        s.shutdown().await;
        r.shutdown().await;
    }

    // Task 2.1 (resume): `ServeComplete` fires on the terminal of the completing
    // payload request EVEN WHEN the resumed fetch requests fewer bytes than the
    // announced `byte_size` — precisely why the design forbids any byte-threshold
    // completion guard. We model a killed-then-resumed transfer deterministically:
    // pre-seed the receiver's store with the LARGER frame's content
    // (content-addressed, so the downloader treats it as already present), then a
    // full fetch pulls only the still-missing smaller frame. That completing
    // payload-carrying request must still route a ServeComplete.
    #[tokio::test]
    async fn resume_fires_serve_complete_despite_partial_bytes() {
        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r = bind_disabled(dr.path()).await;

        let out = s.handle(Role::Out);
        let recv = r.handle(Role::Recv);
        let s_info = out.start().await.unwrap();
        let r_info = recv.start().await.unwrap();
        pair(&s, &s_info, &r, &r_info);

        // frame_a = 4 KiB, frame_b = 64 KiB; announced byte_size = 68 KiB.
        let (pkg_dir, announce) = build_two_frame_package(ds.path(), 4 * 1024, 64 * 1024);
        out.serve(&announce, &pkg_dir, None).await.unwrap();

        // Pre-seed the receiver store with the 64 KiB frame so the fetch resumes,
        // pulling only the 4 KiB frame — far fewer bytes than the 68 KiB byte_size.
        // A byte-threshold completion guard would wrongly withhold ServeComplete
        // here; there must be none.
        let seeded = r
            .store()
            .blobs()
            .add_path(pkg_dir.join("frame_b.fits"))
            .temp_tag()
            .await
            .expect("pre-seed frame_b into the receiver store");

        let mut out_events = out.events().await;
        let mut r_ev = recv.events().await;
        out.announce(r_info.node_id, &announce).await.unwrap();
        let wire = match recv_next(&mut r_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce,
            other => panic!("expected AnnounceReceived, got {other:?}"),
        };

        let fdest = tempdir().unwrap();
        recv.fetch(s_info.node_id, &wire, fdest.path(), crate::sharing::noop_fetch_sink())
            .await
            .unwrap();
        drop(seeded); // the pre-seed temp tag is no longer needed

        let events = collect_until_quiet(
            &mut out_events,
            Duration::from_millis(1500),
            Duration::from_secs(10),
        )
        .await;
        assert!(
            events.iter().any(|e| matches!(
                e,
                TransportEvent::ServeComplete { package_id } if *package_id == announce.package_id
            )),
            "a resumed fetch requesting fewer bytes than byte_size must still fire ServeComplete, got {events:?}"
        );

        s.shutdown().await;
        r.shutdown().await;
    }

    // -----------------------------------------------------------------------
    // Task 2: event demux ((peer, package) ack claims) + pooled control conn.
    // Every test runs two/three real SharedIrohNodes in-process with the relay
    // disabled, paired over localhost direct addresses — CI-safe, no network.
    // -----------------------------------------------------------------------

    use crate::sharing::types::ReceiptOutcome;
    use std::time::Duration;
    use tokio::sync::mpsc::Receiver;

    /// A minimal announce carrying just the ack-correlation `package_id` (the
    /// demux tests never fetch, so the placeholder root_hash is fine).
    fn mk_announce(id: &str) -> PackageAnnounce {
        PackageAnnounce {
            package_id: PackageId(id.to_string()),
            root_hash: "placeholder".to_string(),
            byte_size: 0,
            frame_count: 0,
        }
    }

    fn mk_receipts() -> Vec<FrameReceipt> {
        vec![FrameReceipt {
            frame_uuid: "u".to_string(),
            xxh3: "h".to_string(),
            outcome: ReceiptOutcome::Ingested,
        }]
    }

    async fn recv_next(rx: &mut Receiver<TransportEvent>) -> TransportEvent {
        tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("event channel stalled")
            .expect("event channel closed unexpectedly")
    }

    async fn wait_until<F: FnMut() -> bool>(mut pred: F, timeout: Duration) {
        let start = std::time::Instant::now();
        loop {
            if pred() {
                return;
            }
            if start.elapsed() >= timeout {
                panic!("wait_until timed out after {timeout:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_until_claims(node: &Arc<SharedIrohNode>, want: usize) {
        wait_until(|| node.active_claims() == want, Duration::from_secs(5)).await;
    }

    async fn bind_disabled(dir: &Path) -> Arc<SharedIrohNode> {
        SharedIrohNode::bind(dir, RelayMode::Disabled)
            .await
            .expect("bind relay-disabled node")
    }

    /// Mutually register two paired nodes' addresses (each learns the other's).
    fn pair(a: &Arc<SharedIrohNode>, a_info: &StartInfo, b: &Arc<SharedIrohNode>, b_info: &StartInfo) {
        a.add_peer_ticket(&b_info.pairing_ticket).expect("a pairs b");
        b.add_peer_ticket(&a_info.pairing_ticket).expect("b pairs a");
    }

    // (a) Two Out handles announce to DIFFERENT peers concurrently; each ack
    //     routes to its own announcing handle — no cross-talk.
    #[tokio::test]
    async fn concurrent_acks_route_to_the_right_out_handle() {
        let ds = tempdir().unwrap();
        let d1 = tempdir().unwrap();
        let d2 = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r1 = bind_disabled(d1.path()).await;
        let r2 = bind_disabled(d2.path()).await;

        let out_a = s.handle(Role::Out);
        let out_b = s.handle(Role::Out);
        let recv1 = r1.handle(Role::Recv);
        let recv2 = r2.handle(Role::Recv);

        let s_info = out_a.start().await.unwrap();
        let r1_info = recv1.start().await.unwrap();
        let r2_info = recv2.start().await.unwrap();
        pair(&s, &s_info, &r1, &r1_info);
        pair(&s, &s_info, &r2, &r2_info);

        let mut ack_a = out_a.events().await;
        let mut ack_b = out_b.events().await;
        let mut r1_ev = recv1.events().await;
        let mut r2_ev = recv2.events().await;

        let p1 = mk_announce("pkg-a");
        let p2 = mk_announce("pkg-b");

        // Announce to the two distinct peers (delivery completes when each
        // receiver's Recv consumer receives the announce).
        out_a.announce(r1_info.node_id, &p1).await.unwrap();
        out_b.announce(r2_info.node_id, &p2).await.unwrap();

        // Each receiver observes its own announce, then acks back to the sender.
        let pid1 = match recv_next(&mut r1_ev).await {
            TransportEvent::AnnounceReceived { from, announce } => {
                assert_eq!(from, s_info.node_id);
                announce.package_id
            }
            other => panic!("expected AnnounceReceived on r1, got {other:?}"),
        };
        let pid2 = match recv_next(&mut r2_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce.package_id,
            other => panic!("expected AnnounceReceived on r2, got {other:?}"),
        };
        recv1.ack(s_info.node_id, &pid1, mk_receipts()).await.unwrap();
        recv2.ack(s_info.node_id, &pid2, mk_receipts()).await.unwrap();

        // The acks route to the correct Out handle — no cross-delivery.
        match recv_next(&mut ack_a).await {
            TransportEvent::AckReceived { from, package_id, .. } => {
                assert_eq!(from, r1_info.node_id, "out_a's ack must come from r1");
                assert_eq!(package_id, p1.package_id);
            }
            other => panic!("expected AckReceived on out_a, got {other:?}"),
        }
        match recv_next(&mut ack_b).await {
            TransportEvent::AckReceived { from, package_id, .. } => {
                assert_eq!(from, r2_info.node_id, "out_b's ack must come from r2");
                assert_eq!(package_id, p2.package_id);
            }
            other => panic!("expected AckReceived on out_b, got {other:?}"),
        }
        assert!(ack_a.try_recv().is_err(), "out_a must not receive out_b's ack");
        assert!(ack_b.try_recv().is_err(), "out_b must not receive out_a's ack");
        // Both claims consumed on their acks.
        wait_until_claims(&s, 0).await;

        s.shutdown().await;
        r1.shutdown().await;
        r2.shutdown().await;
    }

    // (b) The SAME package id announced to two peers (Perseus multi-dest shape):
    //     the two `(peer, package)` claims are distinct, and each ack completes
    //     ONLY its own claim.
    #[tokio::test]
    async fn same_package_id_two_peers_each_ack_completes_only_its_claim() {
        let ds = tempdir().unwrap();
        let d1 = tempdir().unwrap();
        let d2 = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r1 = bind_disabled(d1.path()).await;
        let r2 = bind_disabled(d2.path()).await;

        let out = s.handle(Role::Out); // one sender, two destinations
        let recv1 = r1.handle(Role::Recv);
        let recv2 = r2.handle(Role::Recv);

        let s_info = out.start().await.unwrap();
        let r1_info = recv1.start().await.unwrap();
        let r2_info = recv2.start().await.unwrap();
        pair(&s, &s_info, &r1, &r1_info);
        pair(&s, &s_info, &r2, &r2_info);

        let mut acks = out.events().await;
        let mut r1_ev = recv1.events().await;
        let mut r2_ev = recv2.events().await;

        // ONE package id, announced to two peers.
        let pkg = mk_announce("shared-pkg");
        out.announce(r1_info.node_id, &pkg).await.unwrap();
        out.announce(r2_info.node_id, &pkg).await.unwrap();
        assert_eq!(s.active_claims(), 2, "two distinct (peer, package) claims");

        let pid1 = match recv_next(&mut r1_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce.package_id,
            other => panic!("expected AnnounceReceived on r1, got {other:?}"),
        };
        let pid2 = match recv_next(&mut r2_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce.package_id,
            other => panic!("expected AnnounceReceived on r2, got {other:?}"),
        };

        // r1 acks: only the (r1, pkg) claim is consumed.
        recv1.ack(s_info.node_id, &pid1, mk_receipts()).await.unwrap();
        match recv_next(&mut acks).await {
            TransportEvent::AckReceived { from, .. } => assert_eq!(from, r1_info.node_id),
            other => panic!("expected r1 ack, got {other:?}"),
        }
        wait_until_claims(&s, 1).await;

        // r2 acks: the (r2, pkg) claim is consumed too.
        recv2.ack(s_info.node_id, &pid2, mk_receipts()).await.unwrap();
        match recv_next(&mut acks).await {
            TransportEvent::AckReceived { from, .. } => assert_eq!(from, r2_info.node_id),
            other => panic!("expected r2 ack, got {other:?}"),
        }
        wait_until_claims(&s, 0).await;

        s.shutdown().await;
        r1.shutdown().await;
        r2.shutdown().await;
    }

    // (c) An announce arriving with NO Recv consumer registered is an orphan:
    //     the receiver logs the orphan warn and withholds the delivery ack, so
    //     the sender never gets a silent success (it errors/times out → retry).
    #[tokio::test]
    async fn orphan_announce_warns_and_withholds_delivery_ack() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let captured: Arc<std::sync::Mutex<Vec<CapturedEvent>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let _guard = tracing_subscriber::registry()
            .with(CaptureLayer {
                events: captured.clone(),
            })
            .set_default();

        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r = bind_disabled(dr.path()).await;

        let out = s.handle(Role::Out);
        // A Recv handle exists but NEVER takes events() → no Recv consumer.
        let recv = r.handle(Role::Recv);

        let s_info = out.start().await.unwrap();
        let r_info = recv.start().await.unwrap();
        pair(&s, &s_info, &r, &r_info);

        // Take the Out handle's events so its ack claim is registered — the
        // orphan under test is the RECEIVER-side announce with no Recv consumer.
        let _ack = out.events().await;

        let pkg = mk_announce("orphan-pkg");
        let outcome =
            tokio::time::timeout(Duration::from_secs(5), out.announce(r_info.node_id, &pkg)).await;
        assert!(
            !matches!(outcome, Ok(Ok(()))),
            "an orphan announce must not receive a silent delivery ack, got {outcome:?}"
        );

        // The receiver logged the orphan warn naming the event kind.
        wait_until(
            || {
                captured.lock().unwrap().iter().any(|e| {
                    e.message == "inbound event with no consumer"
                        && e.fields.get("kind").map(String::as_str) == Some("announce")
                })
            },
            Duration::from_secs(5),
        )
        .await;

        drop(_guard);
        s.shutdown().await;
        r.shutdown().await;
    }

    // (d) Two sequential control sends to one peer reuse a single pooled
    //     connection (only one real dial).
    #[tokio::test]
    async fn sequential_control_sends_reuse_one_pooled_connection() {
        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r = bind_disabled(dr.path()).await;

        let out = s.handle(Role::Out);
        let recv = r.handle(Role::Recv);

        let s_info = out.start().await.unwrap();
        let r_info = recv.start().await.unwrap();
        pair(&s, &s_info, &r, &r_info);

        let _ack = out.events().await;
        let mut r_ev = recv.events().await; // Recv consumer → announces deliver + ack
        let pkg = mk_announce("pool-pkg");

        out.announce(r_info.node_id, &pkg).await.unwrap();
        out.announce(r_info.node_id, &pkg).await.unwrap();

        // Both delivered to the receiver's single Recv consumer.
        let _ = recv_next(&mut r_ev).await;
        let _ = recv_next(&mut r_ev).await;

        assert_eq!(
            s.control_pool_dials(),
            1,
            "two sequential control sends to one peer must reuse a single pooled connection"
        );

        s.shutdown().await;
        r.shutdown().await;
    }

    // (e) The Task-9 holder probe classifies a refusing holder as `refused` (the
    //     connect handshake succeeds, then the gate closes the connection) and an
    //     address-less holder as `offline`. Two real loopback nodes, relay disabled.
    #[tokio::test]
    async fn probe_holder_classifies_refused_and_offline() {
        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let client = bind_disabled(ds.path()).await;
        let server = bind_disabled(dr.path()).await;

        let c = client.handle(Role::Collab);
        let s = server.handle(Role::Recv);
        let c_info = c.start().await.unwrap();
        let s_info = s.start().await.unwrap();
        pair(&client, &c_info, &server, &s_info);

        // The server refuses every inbound connection at the gate.
        server.set_connect_gate(Arc::new(|_: &NodeId| false));

        // Refused: the connect handshake succeeds, then the gate closes it.
        let refused = client
            .probe_holder(s_info.node_id, false, Duration::from_secs(5))
            .await;
        assert_eq!(
            refused,
            Err(ProbeClass::Refused),
            "a gated holder must classify as refused"
        );

        // Offline: a node id we never learned an address for is undialable.
        let offline = client
            .probe_holder([42u8; 32], false, Duration::from_secs(2))
            .await;
        assert_eq!(
            offline,
            Err(ProbeClass::Offline),
            "an address-less holder must classify as offline"
        );

        client.shutdown().await;
        server.shutdown().await;
    }

    // (f) The pure connect-error classifier (Task 9): a relay-hinted timeout is
    //     `relay_unreachable`; the same timeout with no relay hint is `offline`; an
    //     application close is `refused`; a no-addressing error is `offline`.
    #[test]
    fn classify_connect_err_maps_each_class() {
        assert_eq!(
            classify_connect_err("connection timed out", true),
            ProbeClass::RelayUnreachable
        );
        assert_eq!(
            classify_connect_err("connection timed out", false),
            ProbeClass::Offline
        );
        assert_eq!(
            classify_connect_err("closed by peer: unauthorized", false),
            ProbeClass::Refused
        );
        assert_eq!(
            classify_connect_err("no addressing information available", true),
            ProbeClass::Offline
        );
    }

    // -----------------------------------------------------------------------
    // Task 8: relay-map refresh + idle node rebuild (H2).
    // -----------------------------------------------------------------------

    // (a) A refresh callback returning a CHANGED relay-url set triggers a rebuild
    //     when the node is idle: the node id is STABLE, the shared store survives
    //     (a seeded tag is still there), the T7 reporter's `endpoint_addr()` still
    //     reports the same node id after the internal rebuild, and the role handles
    //     keep working (a full announce→ack round trip completes post-rebuild).
    //     Relay stays Disabled so the in-process endpoints keep dialing over
    //     localhost; the "change" is detected off the resolver-reported url set,
    //     which exercises the whole rebuild path honestly (endpoint + router
    //     re-bound, control pool + watcher rebuilt) without a real relay.
    #[tokio::test]
    async fn relay_refresh_changed_set_rebuilds_when_idle_preserving_identity_store_and_handles() {
        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r = bind_disabled(dr.path()).await;

        let out = s.handle(Role::Out);
        let recv = r.handle(Role::Recv);
        let s_info = out.start().await.unwrap();
        let r_info = recv.start().await.unwrap();
        pair(&s, &s_info, &r, &r_info);

        let id_before = s.node_id();
        // T7 reporter survival groundwork: capture the sender addr before rebuild.
        let addr_before = s.endpoint_addr();
        assert_eq!(*addr_before.id.as_bytes(), id_before, "addr id == node id pre-rebuild");

        // Seed a tag on the shared store to prove the store is NOT rebuilt.
        let tt = s.store().blobs().add_bytes(b"survive".to_vec()).temp_tag().await.unwrap();
        s.store().tags().set("out/pkg/keep", tt.hash_and_format()).await.unwrap();
        drop(tt);

        // A resolver reporting a CHANGED url set (baseline was the empty
        // relay-disabled set), rebuilding to Disabled again.
        let resolver: RelayResolver =
            Arc::new(|| Box::pin(async { Some((RelayMode::Disabled, vec!["marker-v2".to_string()])) }));
        s.start_relay_refresh(resolver);
        s.request_relay_refresh();
        wait_until(|| s.rebuild_count() == 1, Duration::from_secs(10)).await;

        // Node id STABLE + store intact + T7 reporter still reports the new addr
        // (same id) after the internal rebuild.
        assert_eq!(s.node_id(), id_before, "node id must be stable across a relay rebuild");
        assert!(
            tag_present(s.store(), "out/pkg/keep").await,
            "the shared store (and its tag) must survive the rebuild"
        );
        let addr_after = s.endpoint_addr();
        assert_eq!(
            *addr_after.id.as_bytes(),
            id_before,
            "endpoint_addr() (polled by the T7 reporter via Weak) must survive the rebuild and \
             report the same node id"
        );

        // Handles still work post-rebuild: the sender's endpoint changed, so
        // re-teach the receiver our new address, then announce→ack round-trip.
        let s_info2 = out.start().await.unwrap();
        r.add_peer_ticket(&s_info2.pairing_ticket).unwrap();

        let mut acks = out.events().await;
        let mut r_ev = recv.events().await;
        let pkg = mk_announce("post-rebuild");
        out.announce(r_info.node_id, &pkg).await.unwrap();
        let pid = match recv_next(&mut r_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce.package_id,
            other => panic!("expected AnnounceReceived post-rebuild, got {other:?}"),
        };
        recv.ack(s_info2.node_id, &pid, mk_receipts()).await.unwrap();
        match recv_next(&mut acks).await {
            TransportEvent::AckReceived { package_id, .. } => assert_eq!(package_id, pkg.package_id),
            other => panic!("expected AckReceived post-rebuild, got {other:?}"),
        }

        s.shutdown().await;
        r.shutdown().await;
    }

    // (b) A pending rebuild is DEFERRED while an operation is in flight (the idle
    //     gate) and EXECUTES once the node goes idle. Driven directly over the
    //     internal defer machinery (an `op_guard` stands in for a live
    //     serve/fetch/announce) so the defer→release→rebuild ordering is
    //     deterministic, with the node id stable across the deferred rebuild.
    //     Also asserts the TEST-12 completion line + `relay_count` field.
    #[tokio::test]
    async fn relay_rebuild_deferred_while_busy_then_executes_after_release() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let captured: Arc<std::sync::Mutex<Vec<CapturedEvent>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let _guard = tracing_subscriber::registry()
            .with(CaptureLayer {
                events: captured.clone(),
            })
            .set_default();

        let dir = tempdir().unwrap();
        let node = bind_disabled(dir.path()).await;
        let id = node.node_id();

        // Mark the node busy → not idle.
        let op = node.op_guard();
        assert!(!node.is_idle(), "an in-flight op means the node is not idle");

        // A relay change becomes pending; a rebuild attempt while busy must defer.
        node.consider_relay_change(RelayMode::Disabled, vec!["changed".to_string()]);
        node.try_rebuild().await;
        assert_eq!(node.rebuild_count(), 0, "rebuild must defer while an op is in flight");

        // Release the op → idle → the next attempt rebuilds.
        drop(op);
        assert!(node.is_idle(), "released op → idle");
        node.try_rebuild().await;
        assert_eq!(node.rebuild_count(), 1, "rebuild must execute once the node is idle");
        assert_eq!(node.node_id(), id, "node id stable across the deferred rebuild");

        // TEST-12: the success arm now logs a completion line carrying relay_count
        // (was silent). The applied set was `["changed"]` ⇒ relay_count == 1.
        let events = captured.lock().unwrap();
        let complete: Vec<&CapturedEvent> = events
            .iter()
            .filter(|e| e.message == "relay map node rebuild complete")
            .collect();
        assert_eq!(
            complete.len(),
            1,
            "exactly one rebuild-complete line; captured: {:?}",
            events.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        );
        assert_eq!(
            complete[0].fields.get("relay_count").map(String::as_str),
            Some("1"),
            "the completion line must carry relay_count of the applied set; got {:?}",
            complete[0].fields
        );
        drop(events);

        node.shutdown().await;
    }

    // (c) S4 survives the rebuild path: an installed connect gate is re-cloned
    //     into the freshly-built router by `build_router`, so a relay refresh
    //     never silently drops it. A deny-SPECIFIC-peer gate on the receiver is
    //     asserted BOTH ways after a rebuild — the denied peer's inbound connect
    //     is still refused (its announce fails, zero control dispatch), while an
    //     allowed peer is still admitted (the companion that proves the refusal
    //     is the gate's verdict, not broken post-rebuild connectivity).
    #[tokio::test]
    async fn connect_gate_survives_relay_rebuild() {
        let dr = tempdir().unwrap();
        let dd = tempdir().unwrap();
        let da = tempdir().unwrap();
        let r = bind_disabled(dr.path()).await; // gated receiver (will rebuild)
        let denied = bind_disabled(dd.path()).await;
        let allowed = bind_disabled(da.path()).await;

        let recv = r.handle(Role::Recv);
        let out_denied = denied.handle(Role::Out);
        let out_allowed = allowed.handle(Role::Out);

        let r_info = recv.start().await.unwrap();
        let denied_info = out_denied.start().await.unwrap();
        let allowed_info = out_allowed.start().await.unwrap();
        pair(&r, &r_info, &denied, &denied_info);
        pair(&r, &r_info, &allowed, &allowed_info);

        // Deny the denied sender's node id; admit everyone else. Late-bindable —
        // installed on the already-spawned router before the rebuild.
        let denied_id = denied.node_id();
        r.set_connect_gate(Arc::new(move |from: &NodeId| *from != denied_id));

        // Force a relay-refresh rebuild of the receiver's router (deterministic
        // defer machinery: idle node ⇒ the changed url set rebuilds immediately).
        r.consider_relay_change(RelayMode::Disabled, vec!["rebuild-marker".to_string()]);
        r.try_rebuild().await;
        assert_eq!(r.rebuild_count(), 1, "receiver must have rebuilt its router once");

        // Re-teach both senders the receiver's POST-rebuild address (a rebuild
        // rebinds the endpoint ⇒ new direct addrs) so the connections actually
        // reach the NEW router — the refusal below is then the gate's verdict,
        // not unreachability.
        let r_info2 = recv.start().await.unwrap();
        denied.add_peer_ticket(&r_info2.pairing_ticket).unwrap();
        allowed.add_peer_ticket(&r_info2.pairing_ticket).unwrap();

        let mut r_events = recv.events().await;

        // (1) The denied peer is STILL refused after the rebuild.
        let pkg_denied = mk_announce("post-rebuild-denied");
        let outcome = tokio::time::timeout(
            Duration::from_secs(15),
            out_denied.announce(r_info2.node_id, &pkg_denied),
        )
        .await;
        match outcome {
            Ok(Ok(())) => {
                panic!("a denied peer's announce must not succeed after a relay rebuild")
            }
            Ok(Err(_)) | Err(_) => {} // expected: refused → no delivery ack, ever
        }
        // Zero control dispatch from the denied peer (the gate closed the
        // connection before any `Msg` was decoded).
        let never_arrived =
            tokio::time::timeout(Duration::from_millis(300), r_events.recv()).await;
        assert!(
            never_arrived.is_err(),
            "the denied peer must produce zero control dispatch post-rebuild"
        );

        // (2) An allowed peer is STILL admitted after the rebuild.
        let pkg_allowed = mk_announce("post-rebuild-allowed");
        out_allowed
            .announce(r_info2.node_id, &pkg_allowed)
            .await
            .expect("an allowed peer must still be admitted after the relay rebuild");
        match recv_next(&mut r_events).await {
            TransportEvent::AnnounceReceived { from, .. } => assert_eq!(
                from, allowed_info.node_id,
                "the admitted announce must come from the allowed peer"
            ),
            other => panic!("expected AnnounceReceived from the allowed peer, got {other:?}"),
        }

        r.shutdown().await;
        denied.shutdown().await;
        allowed.shutdown().await;
    }

    // A minimal thread-local tracing capture (mirrors the iroh tests.rs layer),
    // scoped to `athenaeum_core::sharing` so the orphan warn can be asserted
    // without touching the global JSONL subscriber.
    #[derive(Clone, Default)]
    struct CapturedEvent {
        message: String,
        fields: std::collections::HashMap<String, String>,
    }

    #[derive(Default)]
    struct FieldCollector {
        message: String,
        fields: std::collections::HashMap<String, String>,
    }

    impl tracing::field::Visit for FieldCollector {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            let rendered = format!("{value:?}");
            if field.name() == "message" {
                self.message = rendered;
            } else {
                self.fields.insert(field.name().to_string(), rendered);
            }
        }
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "message" {
                self.message = value.to_string();
            } else {
                self.fields.insert(field.name().to_string(), value.to_string());
            }
        }
    }

    #[derive(Clone)]
    struct CaptureLayer {
        events: Arc<std::sync::Mutex<Vec<CapturedEvent>>>,
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureLayer {
        fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
            Some(tracing::level_filters::LevelFilter::INFO)
        }

        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if !event.metadata().target().starts_with("athenaeum_core::sharing") {
                return;
            }
            let mut collector = FieldCollector::default();
            event.record(&mut collector);
            self.events.lock().unwrap().push(CapturedEvent {
                message: collector.message,
                fields: collector.fields,
            });
        }
    }
}
