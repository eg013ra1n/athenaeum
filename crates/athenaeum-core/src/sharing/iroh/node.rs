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
//! Event demux and the pooled control connection are **Task 2**: for now every
//! role handle's [`events`](SharingTransport::events) shares the node's single
//! stream, and control messages still connect per message (`// T2 demux`).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use iroh::endpoint::presets;
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

use super::proto::{self, Msg, OfferEntry};
use super::{
    blobs, hex32, spawn_conn_path_diagnostics, ConnectGate, GatedBlobs, SharedConnectGate,
    SyncControlProtocol, CONTROL_SEND_TIMEOUT, EVENT_CHANNEL_CAPACITY, GC_INTERVAL,
    MAX_CONTROL_BYTES, ONLINE_TIMEOUT, SYNC_ALPN,
};

/// Upper bound on the graceful `endpoint.close()` at shutdown (I1). A clean
/// close lets peers see a QUIC close instead of a reset and clears the relay
/// registration promptly; if it stalls we log and move on rather than hang exit.
const SHUTDOWN_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// Sender half of the node's single event stream; cloned into the control
    /// handler. (T2 replaces the single stream with a per-role demux.)
    event_tx: mpsc::Sender<TransportEvent>,
    /// Receiver half, handed out once by the first [`events`](SharingTransport::events) call.
    event_rx: Mutex<Option<mpsc::Receiver<TransportEvent>>>,
    /// Connection-level authorization gate, cloned into both protocol handlers;
    /// the host installs the predicate via [`set_connect_gate`](Self::set_connect_gate).
    connect_gate: SharedConnectGate,
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

        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
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
        // T1 wires no dedup responder into the shared control protocol — the
        // receiver-role responder is installed by Task 3 when it migrates the
        // receiver. A `None` responder answers offers want-all, so nothing is
        // ever silently withheld in the meantime.
        let control = SyncControlProtocol {
            event_tx: event_tx.clone(),
            responder: None,
            gate: Arc::clone(&connect_gate),
        };
        let router = Router::builder(endpoint)
            .accept(iroh_blobs::ALPN, blobs_proto)
            .accept(SYNC_ALPN, control)
            .spawn();

        let endpoint = router.endpoint().clone();
        let node_id: NodeId = *endpoint.id().as_bytes();
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
            event_tx,
            event_rx: Mutex::new(Some(event_rx)),
            connect_gate,
            swept: Mutex::new(HashSet::new()),
            online_waited: AtomicBool::new(false),
            shutdown_done: AtomicBool::new(false),
            relay_watcher: Mutex::new(Some(relay_watcher)),
            key_lock: Mutex::new(Some(key_lock)),
        }))
    }

    /// This endpoint's node id (== ed25519 public key bytes).
    pub fn node_id(&self) -> NodeId {
        self.node_id
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
        Arc::new(RoleHandle {
            node: Arc::clone(self),
            role,
        })
    }

    /// Install the connection-level authorization [`ConnectGate`] (governs BOTH
    /// ALPNs, S4). Overwrites any previously installed gate; left unset, the node
    /// admits every connection.
    pub fn set_connect_gate(&self, gate: ConnectGate) {
        *self.connect_gate.lock().expect("connect_gate mutex poisoned") = Some(gate);
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
            tracing::debug!(error = %e, "shared iroh node router shutdown");
        }
        // Flush the persistent store to disk (also releases the redb file lock
        // so a later re-bind over the same dir can reopen it).
        if let Err(e) = self.store.shutdown().await {
            tracing::debug!(error = %e, "shared iroh node blob store shutdown");
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

    /// Open a control connection to `to`, send one [`Msg`] on a bidi stream, and
    /// wait for the peer's application-level delivery ack before closing. (The
    /// ack-before-close is what makes the connection safe to tear down; T2 pools
    /// this connection instead of re-dialing per message.)
    async fn send_control(&self, to: NodeId, msg: Msg) -> Result<()> {
        let target = self.dial_target(to)?;
        let bytes = msg.encode()?;
        let endpoint = &self.endpoint;

        let send = async {
            let conn = endpoint
                .connect(target, SYNC_ALPN)
                .await
                .context("connect sync control channel")?;
            spawn_conn_path_diagnostics(&conn, "outgoing");
            let (mut tx, mut rx) = conn.open_bi().await.context("open control stream")?;
            tx.write_all(&bytes).await.context("write control message")?;
            tx.finish().context("finish control stream")?;
            let ack = rx.read_to_end(8).await.context("await control delivery ack")?;
            if ack.is_empty() {
                anyhow::bail!("control message not acknowledged by peer");
            }
            conn.close(0u32.into(), b"ok");
            anyhow::Ok(())
        };

        tokio::time::timeout(CONTROL_SEND_TIMEOUT, send)
            .await
            .map_err(|_| anyhow!("sync control send to {} timed out", hex32(&to)))??;
        Ok(())
    }

    /// Open a control connection to `to`, send one request [`Msg`], and read the
    /// peer's **reply `Msg`** off the same bidi stream (the request/response
    /// counterpart of [`send_control`](Self::send_control)). Used by the dedup
    /// handshake; each round drives its own connection/stream.
    async fn send_request(&self, to: NodeId, msg: Msg) -> Result<Msg> {
        let target = self.dial_target(to)?;
        let bytes = msg.encode()?;
        let endpoint = &self.endpoint;

        let exchange = async {
            let conn = endpoint
                .connect(target, SYNC_ALPN)
                .await
                .context("connect sync control channel")?;
            spawn_conn_path_diagnostics(&conn, "outgoing");
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
            conn.close(0u32.into(), b"ok");
            anyhow::Ok(reply)
        };

        match tokio::time::timeout(CONTROL_SEND_TIMEOUT, exchange).await {
            Ok(result) => result,
            Err(_) => Err(anyhow!("sync control request to {} timed out", hex32(&to))),
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

        let _ = self.event_tx.try_send(TransportEvent::FetchProgress {
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

    fn take_events(&self) -> mpsc::Receiver<TransportEvent> {
        let mut guard = self.event_rx.lock().expect("event_rx mutex poisoned");
        match guard.take() {
            Some(rx) => rx,
            None => {
                let (_tx, rx) = mpsc::channel(1);
                rx
            }
        }
    }
}

/// A per-[`Role`] view onto a [`SharedIrohNode`], implementing
/// [`SharingTransport`] with role-prefixed blob tags (Д3). Cheap to clone
/// (an `Arc` + a `Copy` role). The node it references is torn down by the host
/// via [`SharedIrohNode::shutdown`], not by any one handle.
pub struct RoleHandle {
    node: Arc<SharedIrohNode>,
    role: Role,
}

impl RoleHandle {
    /// The shared node's blob store (used by tests to prove two role handles read
    /// the SAME store instance).
    #[allow(dead_code)] // consumed by the shared-store test below
    pub(crate) fn store(&self) -> &Store {
        self.node.store()
    }
}

#[async_trait]
impl SharingTransport for RoleHandle {
    async fn start(&self) -> Result<StartInfo> {
        self.node.role_start(self.role).await
    }

    async fn announce(&self, to: NodeId, a: &PackageAnnounce) -> Result<()> {
        self.node.role_announce(self.role, to, a).await
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
        self.node
            .role_announce_project(self.role, to, project_id, package_id, announce)
            .await
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

    async fn events(&self) -> mpsc::Receiver<TransportEvent> {
        // T2 demux: for now every role handle shares the node's single event
        // stream (single-consumer, taken once). Task 2 replaces this with a
        // per-(peer, package id) ack-claim registry + one registered receiver
        // consumer for inbound announces.
        self.node.take_events()
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
}
