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
//! that module's construction shape, protocol handlers ([`SyncControlProtocol`],
//! [`GatedBlobs`]), blob glue ([`blobs`]), and connection-path diagnostics
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
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
use iroh_blobs::{BlobsProtocol, Hash};
use iroh_tickets::endpoint::EndpointTicket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::account::keys::{device_key_path, DeviceKey, DeviceKeyLock};
use crate::sharing::types::{
    FrameReceipt, NodeId, PackageAnnounce, PackageId, StartInfo, TransportEvent,
};
use crate::sharing::SharingTransport;
use crate::sync::DedupResponder;

use super::proto::{self, Msg, OfferEntry};
use super::{
    blobs, hex32, spawn_conn_path_diagnostics, ConnectGate, Delivery, EventSink, GatedBlobs,
    SharedConnectGate, SharedResponder, SyncControlProtocol, CONTROL_SEND_TIMEOUT,
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
                // Self-generated; never routed through the accept path.
                TransportEvent::FetchProgress { .. } => None,
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

    /// Best-effort delivery of a self-generated [`TransportEvent::FetchProgress`]
    /// to the Recv consumer (UI data; never blocks, dropped if the channel is
    /// full or no consumer is registered).
    fn emit_fetch_progress(&self, event: TransportEvent) {
        if let Some((_, tx)) = self
            .inner
            .lock()
            .expect("demux mutex poisoned")
            .recv
            .as_ref()
        {
            let _ = tx.try_send(event);
        }
    }

    /// Number of live ack claims (test introspection).
    #[cfg(test)]
    fn claim_count(&self) -> usize {
        self.inner.lock().expect("demux mutex poisoned").claims.len()
    }
}

/// The inbound-event variant an orphan warn names, plus the peer it came from.
fn event_kind_and_peer(event: &TransportEvent) -> (&'static str, NodeId) {
    match event {
        TransportEvent::AnnounceReceived { from, .. } => ("announce", *from),
        TransportEvent::AckReceived { from, .. } => ("ack", *from),
        TransportEvent::ProjectAnnounceReceived { from, .. } => ("project_announce", *from),
        TransportEvent::ProjectRequestReceived { from, .. } => ("project_request", *from),
        TransportEvent::FetchProgress { .. } => ("fetch_progress", [0u8; 32]),
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

/// The one iroh endpoint/router/store for this process (see module docs).
pub struct SharedIrohNode {
    /// Endpoint handle (clone of the router's); dials peers + downloads blobs.
    endpoint: Endpoint,
    /// Keeps the accept loop (both protocols) alive; torn down in [`shutdown`](Self::shutdown).
    router: Router,
    /// The single blob store shared by every role handle.
    store: Store,
    /// This endpoint's node id (== ed25519 public key bytes).
    node_id: NodeId,
    /// Known peer addresses (from pairing), used to dial the control channel.
    peers: Mutex<HashMap<NodeId, EndpointAddr>>,
    /// Endpoint address lookup — consumed by the blobs downloader when it dials
    /// by node id. Cloned handle; shares state with the endpoint.
    lookup: iroh::address_lookup::memory::MemoryLookup,
    /// Prefixed package tag (`<role>/pkg/<id>`) → collection hash, registered by
    /// [`serve`](SharingTransport::serve), injected into the wire announce by
    /// [`announce`](SharingTransport::announce). Keyed by the FULL prefixed tag
    /// so two roles serving the same package id never collide.
    served: Mutex<HashMap<String, Hash>>,
    /// Whether a relay is configured. Gates the `online()` wait — with the relay
    /// disabled there is no home relay and `online()` would hang.
    uses_relay: bool,
    /// The relay URLs the endpoint was built with (H1 reporting groundwork, T7).
    relay_urls: Vec<String>,
    /// Per-`(peer, package)` ack-claim + Recv-consumer router (Task 2, Д4). Owns
    /// the fan-out of the node's single inbound event stream; cloned into the
    /// control-protocol handler (as [`EventSink::Demux`]) and consulted by every
    /// role handle's [`events`](SharingTransport::events) /
    /// [`announce`](SharingTransport::announce).
    demux: Arc<EventDemux>,
    /// Per-peer pooled control connections (Task 2), reused across control sends.
    control_pool: Arc<ControlPool>,
    /// Monotonic id source for role handles, so the demux can attribute a claim /
    /// the Recv consumer to a specific handle and release them all on its drop.
    next_handle_id: AtomicU64,
    /// Connection-level authorization gate, cloned into both protocol handlers;
    /// the host installs the predicate via [`set_connect_gate`](Self::set_connect_gate).
    connect_gate: SharedConnectGate,
    /// Dedup responder slot, cloned into the control protocol handler; the
    /// receiver installs it via [`set_dedup_responder`](Self::set_dedup_responder)
    /// when it migrates onto the node (Task 3). Empty ⇒ inbound offers are
    /// answered want-all, so nothing is ever silently withheld.
    responder: SharedResponder,
    /// Role prefixes whose startup sweep has already run (once per prefix).
    swept: Mutex<HashSet<&'static str>>,
    /// Whether the one-shot home-relay `online()` wait has run.
    online_waited: AtomicBool,
    /// Whether [`shutdown`](Self::shutdown) has already run (idempotency guard).
    shutdown_done: AtomicBool,
    /// Home-relay status watcher task; aborted at shutdown (iroh keeps the
    /// watcher alive until the last endpoint clone drops, so we abort explicitly).
    relay_watcher: Mutex<Option<JoinHandle<()>>>,
    /// The held device-key advisory lock (I4); dropped at shutdown so a re-bind
    /// can re-acquire it.
    key_lock: Mutex<Option<DeviceKeyLock>>,
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
        let secret_key = SecretKey::from_bytes(&key.secret_bytes());
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
        let blobs_proto = GatedBlobs {
            inner: BlobsProtocol::new(&store, None),
            gate: Arc::clone(&connect_gate),
        };
        // Late-bindable dedup responder slot (Д3 shape): empty at bind, installed
        // by the receiver when it migrates onto the node (`set_dedup_responder`,
        // Task 3). Empty ⇒ offers are answered want-all, so nothing is ever
        // silently withheld before the receiver wires its catalog responder.
        // Inbound events fan out through the demux (Task 2, Д4), not a single
        // shared stream.
        let responder: SharedResponder = Arc::new(Mutex::new(None));
        let control = SyncControlProtocol {
            sink: EventSink::Demux(Arc::clone(&demux)),
            responder: Arc::clone(&responder),
            gate: Arc::clone(&connect_gate),
        };
        let router = Router::builder(endpoint)
            .accept(iroh_blobs::ALPN, blobs_proto)
            .accept(SYNC_ALPN, control)
            .spawn();

        let endpoint = router.endpoint().clone();
        let node_id: NodeId = *endpoint.id().as_bytes();
        let control_pool = ControlPool::new(endpoint.clone());
        let relay_watcher = spawn_home_relay_watcher(endpoint.clone());

        tracing::debug!(node_id = %endpoint.id().fmt_short(), "shared iroh node bound");
        Ok(Arc::new(Self {
            endpoint,
            router,
            store,
            node_id,
            peers: Mutex::new(HashMap::new()),
            lookup,
            served: Mutex::new(HashMap::new()),
            uses_relay,
            relay_urls,
            demux,
            control_pool,
            next_handle_id: AtomicU64::new(0),
            connect_gate,
            swept: Mutex::new(HashSet::new()),
            online_waited: AtomicBool::new(false),
            shutdown_done: AtomicBool::new(false),
            relay_watcher: Mutex::new(Some(relay_watcher)),
            key_lock: Mutex::new(Some(key_lock)),
            responder,
        }))
    }

    /// This endpoint's node id (== ed25519 public key bytes).
    pub fn node_id(&self) -> NodeId {
        self.node_id
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
        self.control_pool.dials.load(Ordering::Relaxed)
    }

    /// This endpoint's current [`EndpointAddr`] (direct addrs + relay url) — the
    /// self-reported address for H1 (Task 7). Call after a handle's
    /// [`start`](SharingTransport::start) so address discovery has settled.
    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// The relay URLs the endpoint was built with (H1 groundwork, Task 7).
    pub fn relay_urls(&self) -> Vec<String> {
        self.relay_urls.clone()
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
        match crate::sync::pairing::peer_addr_with_relays(from, &self.relay_urls) {
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

    /// Gracefully tear down the node (I1): abort the relay watcher, then
    /// `Router::shutdown().await` → store shutdown → bounded `endpoint.close()`,
    /// then release the device-key lock. Idempotent — a second call is a no-op.
    pub async fn shutdown(&self) {
        if self.shutdown_done.swap(true, Ordering::SeqCst) {
            return;
        }
        // Abort the relay watcher first: iroh keeps the `home_relay_status`
        // watcher alive until the last endpoint clone drops (closing the
        // endpoint does NOT stop it), so it must be aborted explicitly.
        if let Some(handle) = self
            .relay_watcher
            .lock()
            .expect("relay_watcher mutex poisoned")
            .take()
        {
            handle.abort();
        }
        if let Err(e) = self.router.shutdown().await {
            tracing::warn!(error = %e, "shared iroh node router shutdown");
        }
        // Flush the persistent store to disk (also releases the redb file lock
        // so a later re-bind over the same dir can reopen it).
        if let Err(e) = self.store.shutdown().await {
            tracing::warn!(error = %e, "shared iroh node blob store shutdown");
        }
        if tokio::time::timeout(SHUTDOWN_CLOSE_TIMEOUT, self.endpoint.close())
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

    /// Open a bidi stream on the peer's pooled control connection, send one
    /// [`Msg`], and wait for the peer's application-level delivery ack. The
    /// connection is NOT closed after the send — it stays pooled for reuse (Task
    /// 2); the ack-before-return semantics are unchanged verbatim. Any error
    /// invalidates the pooled entry so the next send re-dials.
    async fn send_control(&self, to: NodeId, msg: Msg) -> Result<()> {
        let target = self.dial_target(to)?;
        let bytes = msg.encode()?;
        let conn = self.control_pool.get_or_connect(to, target).await?;

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
                self.control_pool.invalidate(to, &conn);
                Err(e)
            }
            Err(_) => {
                self.control_pool.invalidate(to, &conn);
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
        let conn = self.control_pool.get_or_connect(to, target).await?;

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
                self.control_pool.invalidate(to, &conn);
                Err(e)
            }
            Err(_) => {
                self.control_pool.invalidate(to, &conn);
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
        if self.uses_relay && !self.online_waited.swap(true, Ordering::SeqCst) {
            match tokio::time::timeout(ONLINE_TIMEOUT, self.endpoint.online()).await {
                Ok(()) => {
                    let relay_url = self
                        .endpoint
                        .addr()
                        .relay_urls()
                        .next()
                        .map(|u| u.to_string());
                    tracing::info!(
                        node_id = %self.endpoint.id().fmt_short(),
                        relay_url = relay_url.as_deref().unwrap_or("unknown"),
                        "home relay connected"
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        node_id = %self.endpoint.id().fmt_short(),
                        timeout_ms = ONLINE_TIMEOUT.as_millis() as u64,
                        "home relay wait timed out; proceeding on direct addresses only (unreachable behind NAT)"
                    );
                }
            }
        }

        let addr = self.endpoint.addr();
        let pairing_ticket = EndpointTicket::from(addr).to_string();
        tracing::debug!(role = prefix, node_id = %self.endpoint.id().fmt_short(), "shared iroh node role started");
        Ok(StartInfo {
            node_id: self.node_id,
            pairing_ticket,
        })
    }

    async fn role_announce(&self, role: Role, to: NodeId, a: &PackageAnnounce) -> Result<()> {
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
    ) -> Result<()> {
        let root_hash: Hash = pkg.root_hash.parse().with_context(|| {
            format!("parse collection hash from announce root_hash {:?}", pkg.root_hash)
        })?;
        let provider =
            EndpointId::from_bytes(&from).map_err(|e| anyhow!("invalid provider node id: {e}"))?;
        let tag = role_package_tag(role.prefix(), &pkg.package_id);
        blobs::fetch_collection_to_dir(
            &self.store,
            &self.endpoint,
            provider,
            root_hash,
            &tag,
            dest_dir,
        )
        .await?;

        self.demux.emit_fetch_progress(TransportEvent::FetchProgress {
            package_id: pkg.package_id.clone(),
            bytes_done: pkg.byte_size,
            bytes_total: pkg.byte_size,
        });
        tracing::debug!(from = %hex32(&from), package_id = %pkg.package_id.0, "iroh fetch complete");
        Ok(())
    }

    async fn role_serve(
        &self,
        role: Role,
        pkg: &PackageAnnounce,
        src_dir: &Path,
        want: Option<&HashSet<String>>,
    ) -> Result<()> {
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
    ) -> Result<()> {
        self.node.role_fetch(self.role, from, pkg, dest_dir).await
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
fn spawn_home_relay_watcher(endpoint: Endpoint) -> JoinHandle<()> {
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
                if connected {
                    tracing::info!(relay_url = %url, "home relay connected");
                } else if let Some(err) = st.last_error() {
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
