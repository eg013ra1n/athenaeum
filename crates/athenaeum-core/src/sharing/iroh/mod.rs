//! Real peer-to-peer transport over [iroh] (task A5).
//!
//! [`IrohTransport`] implements the same [`SharingTransport`] contract as the
//! in-process [`LoopbackTransport`](crate::sharing::loopback::LoopbackTransport)
//! mock, so the sync engine (task A4) runs unchanged over a real QUIC network.
//! It composes two iroh protocols on one endpoint:
//!
//! - **iroh-blobs** ([`iroh_blobs::ALPN`]) moves package *content*. A package
//!   directory is imported as a content-addressed [collection]; a fetch
//!   downloads that collection (verified, resumable) and rebuilds the directory.
//!   See [`blobs`].
//! - **a custom control protocol** ([`SYNC_ALPN`]) moves the *metadata* —
//!   [`PackageAnnounce`]s and per-frame acks — as postcard [`Msg`]s over
//!   bidirectional QUIC streams (a one-byte reply confirms in-process delivery
//!   before the connection closes). See [`proto`].
//!
//! # Root hash
//!
//! The engine builds a [`PackageAnnounce`] with a placeholder `root_hash`. On
//! [`serve`](SharingTransport::serve) the transport imports the package into its
//! blob store and remembers the resulting collection hash; on
//! [`announce`](SharingTransport::announce) it substitutes that hash into the
//! wire announce, so the receiver's [`fetch`](SharingTransport::fetch) downloads
//! by it. The `package_id` is preserved unchanged, so ack correlation in the
//! engine is unaffected.
//!
//! # Peer addressing
//!
//! The trait addresses peers by [`NodeId`] (== the iroh endpoint's ed25519
//! public key). Endpoints learn each other's dialable address out of band via a
//! pairing ticket ([`StartInfo::pairing_ticket`], an [`EndpointTicket`]): each
//! side calls [`add_peer_ticket`](IrohTransport::add_peer_ticket) once. The
//! address feeds both the control channel and the blobs downloader through the
//! endpoint's in-memory address lookup — no dependence on external discovery,
//! which is what lets the in-process tests run with the relay disabled.
//!
//! [iroh]: https://docs.rs/iroh
//! [collection]: iroh_blobs::format::collection::Collection

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use iroh::endpoint::{presets, Connection};
use iroh::protocol::{ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey};
use iroh_blobs::api::Store;
use iroh_blobs::store::fs::options::Options as FsOptions;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::store::mem::MemStore;
use iroh_blobs::store::GcConfig;
use iroh_blobs::{BlobsProtocol, Hash};
use iroh_tickets::endpoint::EndpointTicket;
use tokio::sync::mpsc;

use super::types::{FrameReceipt, NodeId, PackageAnnounce, PackageId, StartInfo, TransportEvent};
use super::SharingTransport;
use crate::sync::DedupResponder;

pub mod blobs;
pub mod proto;

#[cfg(test)]
mod tests;

use proto::{Msg, OfferEntry};

/// Custom ALPN for the announce/ack control channel. Distinct from
/// [`iroh_blobs::ALPN`] so the two protocols coexist on one endpoint.
pub const SYNC_ALPN: &[u8] = b"athenaeum/sync/1";

/// Deterministic blob-store tag for a package collection. `release` deletes by
/// this exact name, so both the import (serve) and download (fetch) sides pin
/// with it — never with an auto-named tag.
pub(crate) fn package_tag(package_id: &PackageId) -> String {
    format!("pkg/{}", package_id.0)
}

/// How often the fs blob store's GC loop runs. A partial download older than one
/// interval may be collected before a resume — that degrades resume to a
/// re-download, never loses data (every byte is re-verified). 900 s is a
/// deliberately slack interval so a normal transfer never races collection.
const GC_INTERVAL: Duration = Duration::from_secs(900);

/// Depth of an endpoint's inbound event channel. Control events are low volume;
/// this comfortably holds bursts of announces/acks.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Upper bound on one control message. Announces are tiny; an ack carries one
/// receipt per frame, so this is generous headroom for a large package.
const MAX_CONTROL_BYTES: usize = 16 * 1024 * 1024;

/// Cap on a single control-channel send (connect + write + delivery ack) so a
/// dead peer surfaces as a retryable error instead of wedging the engine.
const CONTROL_SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound on waiting for a home relay in [`start`](SharingTransport::start). With
/// a relay configured this is ample for it to connect; if it can't, we proceed
/// on direct addresses rather than hang. Never reached with the relay disabled
/// (that path skips the wait entirely — there is no home relay to connect to,
/// and [`Endpoint::online`] would otherwise block forever).
const ONLINE_TIMEOUT: Duration = Duration::from_secs(10);

/// A connection-level authorization predicate over a dialing peer's node id
/// (collab exchange, slice 4). Installed via
/// [`IrohTransport::set_connect_gate`] and shared (behind a mutexed `Option`) by
/// the control-channel and gated-blobs handlers. `true` ⇒ admit; `false` ⇒ close
/// the connection before any control dispatch or blob byte. A transport with no
/// gate installed accepts every connection (today's behavior — Perseus + sender
/// transports leave it unset).
pub type ConnectGate = Arc<dyn Fn(&NodeId) -> bool + Send + Sync>;

/// Shared, late-bindable slot for the [`ConnectGate`]. Cloned into both protocol
/// handlers at construction; the host installs the actual predicate afterwards
/// via [`IrohTransport::set_connect_gate`], so an already-spawned router picks it
/// up without a rebuild.
type SharedConnectGate = Arc<Mutex<Option<ConnectGate>>>;

/// Evaluate the shared connect gate for `from`. Absent gate ⇒ admit (accept-all,
/// today's behavior). The gate `Arc` is cloned out from under the lock BEFORE the
/// predicate runs, so a gate that does blocking catalog I/O never holds the mutex
/// across that work.
fn connect_gate_admits(gate: &SharedConnectGate, from: &NodeId) -> bool {
    let predicate = gate.lock().expect("connect_gate mutex poisoned").clone();
    match predicate {
        Some(g) => g(from),
        None => true,
    }
}

/// Where an [`IrohTransport`] keeps downloaded/served blob content.
pub enum BlobStore {
    /// Ephemeral in-memory store (tests, or a stateless hop).
    Memory,
    /// Persistent fs-backed store; content lives under `<dir>/sync_blobs`.
    Fs(PathBuf),
}

/// A peer-to-peer sharing transport backed by an iroh endpoint.
pub struct IrohTransport {
    /// Endpoint handle (clone of the router's), used to dial peers + download.
    endpoint: Endpoint,
    /// Keeps the accept loop (both protocols) alive; aborts on drop.
    router: Router,
    /// Blob store shared by the blobs protocol handler and our fetch/serve calls.
    store: Store,
    /// This endpoint's node id (== ed25519 public key bytes).
    node_id: NodeId,
    /// Known peer addresses (from pairing), used to dial the control channel.
    peers: Mutex<HashMap<NodeId, EndpointAddr>>,
    /// Endpoint address lookup — same peer info, consumed by the blobs downloader
    /// when it dials by node id. Cloned handle; shares state with the endpoint.
    lookup: iroh::address_lookup::memory::MemoryLookup,
    /// `package_id` → collection hash registered by [`serve`](SharingTransport::serve),
    /// injected into the wire announce by [`announce`](SharingTransport::announce).
    served: Mutex<HashMap<String, Hash>>,
    /// Whether a relay is configured. Gates the `online()` wait in `start` — with
    /// the relay disabled there is no home relay and `online()` would hang.
    uses_relay: bool,
    /// Sender half of this endpoint's event stream; cloned into the control handler.
    event_tx: mpsc::Sender<TransportEvent>,
    /// Receiver half, handed out once by [`events`](SharingTransport::events).
    event_rx: Mutex<Option<mpsc::Receiver<TransportEvent>>>,
    /// Connection-level authorization gate (collab exchange, slice 4). Cloned
    /// into both protocol handlers at construction; the host installs the
    /// predicate later via [`set_connect_gate`](Self::set_connect_gate).
    connect_gate: SharedConnectGate,
}

impl IrohTransport {
    /// Build and bind a transport from a persisted 32-byte secret key.
    ///
    /// `relay_mode` is [`RelayMode::Default`] for production (n0 relays for NAT
    /// traversal) or [`RelayMode::Disabled`] for direct-only / in-process tests.
    /// The endpoint binds immediately; call [`start`](SharingTransport::start) to
    /// wait until it is online and obtain its pairing ticket.
    /// `responder` answers inbound dedup `Offer`/`FullHashes` requests on the
    /// control channel (a running receiver passes `Some(CatalogDedupResponder)`;
    /// a send-only endpoint passes `None`, in which case peers get a want-all
    /// reply so nothing is silently withheld).
    pub async fn new(
        secret: [u8; 32],
        relay_mode: RelayMode,
        store: BlobStore,
        responder: Option<Arc<dyn DedupResponder>>,
    ) -> Result<Self> {
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let secret_key = SecretKey::from_bytes(&secret);
        let uses_relay = !matches!(relay_mode, RelayMode::Disabled);
        // Snapshot what the ENDPOINT is being built with, before the builder
        // consumes `relay_mode`. This is deliberately the transport-level view:
        // `sync::pairing` logs the relay map it *resolved* from the hub, but a
        // cached map (or a dev `RelayMode::Default` fallback) means the endpoint
        // can end up on a different set — so this line states the actual build.
        let relay_mode_label = match &relay_mode {
            RelayMode::Disabled => "disabled",
            RelayMode::Default => "default",
            RelayMode::Staging => "staging",
            RelayMode::Custom(_) => "custom",
        };
        let relay_count = relay_mode.relay_map().len();
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
            "iroh endpoint relay configuration"
        );

        let store: Store = match store {
            BlobStore::Memory => MemStore::new().into(),
            BlobStore::Fs(dir) => {
                let blob_dir = dir.join("sync_blobs");
                // Mirror FsStore::load's internals but with GC on: load() hardcodes
                // gc: None, so no GC loop would ever run and released blobs would
                // leak forever. The interval is slack (see GC_INTERVAL) so an
                // in-flight transfer never races collection.
                std::fs::create_dir_all(&blob_dir)
                    .with_context(|| format!("create blob dir {}", blob_dir.display()))?;
                let db_path = blob_dir.join("blobs.db");
                let mut options = FsOptions::new(&blob_dir);
                options.gc = Some(GcConfig {
                    interval: GC_INTERVAL,
                    add_protected: None,
                });
                FsStore::load_with_opts(db_path, options)
                    .await
                    .with_context(|| format!("open blob store {}", blob_dir.display()))?
                    .into()
            }
        };

        // Both protocols on one router: blobs for content, our control ALPN for
        // announce/ack. `spawn` registers both ALPNs on the endpoint.
        //
        // Finding F5 (was accepted residual, LOW; now hardened by the slice-4
        // connect gate below): the blobs provider serves any stored blob to any
        // node that requests it BY HASH. Unauthorized *ingestion* is blocked
        // receiver-side by the H1 peer-authorization gate in `sync::receiver`,
        // and pulling a blob additionally requires the collection `root_hash` (an
        // unguessable BLAKE3 digest sent only to the authorized peer over
        // encrypted QUIC, released promptly after ack). Rather than the
        // iroh-blobs `ConnectMode::Intercept` hook, the connect gate is enforced
        // at the `ProtocolHandler` layer (below): a host-installed predicate the
        // transport merely stores a slot for — the authorization *state* still
        // lives in the host closure, not the transport.
        // Shared connect gate: unset at construction, installed later by the host
        // (`set_connect_gate`). Cloned into BOTH handlers so a single install
        // point governs the control channel AND the blobs provider.
        let connect_gate: SharedConnectGate = Arc::new(Mutex::new(None));

        // Wrap the blobs provider so an ungated peer never receives a blob byte:
        // `GatedBlobs` checks the connect gate against the dialing node id before
        // delegating to the inner `iroh_blobs` handler (finding F5 hardening).
        let blobs = GatedBlobs {
            inner: BlobsProtocol::new(&store, None),
            gate: Arc::clone(&connect_gate),
        };
        let control = SyncControlProtocol {
            event_tx: event_tx.clone(),
            responder,
            gate: Arc::clone(&connect_gate),
        };
        let router = Router::builder(endpoint)
            .accept(iroh_blobs::ALPN, blobs)
            .accept(SYNC_ALPN, control)
            .spawn();

        let endpoint = router.endpoint().clone();
        let node_id: NodeId = *endpoint.id().as_bytes();

        tracing::debug!(node_id = %endpoint.id().fmt_short(), "iroh transport bound");
        Ok(Self {
            endpoint,
            router,
            store,
            node_id,
            peers: Mutex::new(HashMap::new()),
            lookup,
            served: Mutex::new(HashMap::new()),
            uses_relay,
            event_tx,
            event_rx: Mutex::new(Some(event_rx)),
            connect_gate,
        })
    }

    /// This endpoint's node id, available before [`start`](SharingTransport::start).
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Install the connection-level authorization [`ConnectGate`] (collab
    /// exchange, slice 4). Governs BOTH the control channel and the blobs
    /// provider: a peer the gate refuses is closed before any control dispatch or
    /// blob byte. Overwrites any previously installed gate; passing is idempotent.
    /// Left unset, the transport admits every connection (Perseus + sender
    /// transports never call this).
    pub fn set_connect_gate(&self, gate: ConnectGate) {
        *self.connect_gate.lock().expect("connect_gate mutex poisoned") = Some(gate);
    }

    /// This endpoint's current [`EndpointAddr`] (direct addrs + relay url). Call
    /// after [`start`](SharingTransport::start) so address discovery has settled.
    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// Register a peer's dialable address (from a received pairing ticket or an
    /// out-of-band exchange), enabling the control channel and the blobs
    /// downloader to reach it.
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

    /// Gracefully tear down the endpoint + router (tests). Consumes `self`.
    pub async fn shutdown(self) {
        if let Err(e) = self.router.shutdown().await {
            tracing::debug!(error = %e, "iroh router shutdown");
        }
        // Flush a persistent store to disk; a no-op for the memory store.
        if let Err(e) = self.store.shutdown().await {
            tracing::debug!(error = %e, "iroh blob store shutdown");
        }
    }

    /// Resolve a peer node id to a dialable target: the full addr from the peer
    /// book when known, else the bare id (resolved via address lookup).
    fn dial_target(&self, to: NodeId) -> Result<EndpointAddr> {
        if let Some(addr) = self.peers.lock().expect("peers mutex poisoned").get(&to) {
            return Ok(addr.clone());
        }
        let id = EndpointId::from_bytes(&to).map_err(|e| anyhow!("invalid peer node id: {e}"))?;
        Ok(EndpointAddr::new(id))
    }

    /// Open a control connection to `to`, send one [`Msg`] on a bidirectional
    /// stream, and wait for the peer's application-level delivery ack before
    /// closing.
    ///
    /// A bidi request/ack (rather than a bare uni write) is deliberate: the
    /// 1-byte ack means the receiver has *dispatched* the message in-process, so
    /// we never tear the connection down while data is still buffered unread on
    /// the far side. The brief's "uni stream" is shape guidance; correctness of
    /// delivery-before-close wins.
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
            // The receiver writes a one-byte ack only after handing the event to
            // the engine; an empty read means it closed without dispatching.
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
    /// peer's **reply `Msg`** (decoded off the same bidi stream) — the
    /// request/response counterpart of [`send_control`](Self::send_control),
    /// whose reply is only the one-byte delivery ack. Used by the dedup
    /// handshake, where the peer answers each request with a [`Msg::Want`]. The
    /// responder is stateless, so each round drives its own connection/stream.
    ///
    /// Any connect/write/read/decode/timeout error propagates so the caller's
    /// [`negotiate_want`](SharingTransport::negotiate_want) returns `Err` and the
    /// engine falls back to a full announce.
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
            // Read the peer's reply Msg (it finishes its send half after writing).
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
}

#[async_trait]
impl SharingTransport for IrohTransport {
    async fn start(&self) -> Result<StartInfo> {
        // Startup sweep: every tag in this store is stale — PackageIds are
        // per-process (crash-resume re-announces with fresh ids and re-serves
        // from source dirs), and receiver fetch-tags never outlive an ack.
        // Also retires the pre-Stage-1.5 auto-named tags on existing stores.
        match self.store.tags().delete_all().await {
            Ok(removed) if removed > 0 => {
                tracing::info!(count = removed, "blob store startup sweep removed stale tags")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "blob store startup sweep failed"),
        }

        // With a relay configured, wait (bounded) for it to connect so the addr
        // carries a relay url for NAT traversal. With the relay disabled there is
        // no home relay — `online()` would hang — and the direct addresses bound
        // at construction are already dialable, so we skip the wait. Idempotent.
        // Both outcomes are logged: which home relay we landed on, or that we are
        // proceeding without one (behind NAT that means unreachable) — the single
        // most important startup fact for a NAT-traversal investigation.
        if self.uses_relay {
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
        tracing::debug!(node_id = %self.endpoint.id().fmt_short(), "iroh endpoint online");
        Ok(StartInfo {
            node_id: self.node_id,
            pairing_ticket,
        })
    }

    async fn announce(&self, to: NodeId, a: &PackageAnnounce) -> Result<()> {
        // Substitute the collection hash registered by `serve` so the receiver
        // can download by it; keep everything else (crucially `package_id`).
        let mut wire = a.clone();
        {
            let served = self.served.lock().expect("served mutex poisoned");
            match served.get(&a.package_id.0) {
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

    async fn announce_project(
        &self,
        to: NodeId,
        project_id: &str,
        package_id: &str,
        a: &PackageAnnounce,
    ) -> Result<()> {
        // Same served-collection-hash substitution as `announce`: swap in the
        // collection hash `serve` registered so the receiver can download by it,
        // keeping the engine-minted `announce.package_id` (ack correlation).
        let mut wire = a.clone();
        {
            let served = self.served.lock().expect("served mutex poisoned");
            match served.get(&a.package_id.0) {
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

    async fn request_project(
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

    async fn fetch(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
    ) -> Result<()> {
        let root_hash: Hash = pkg.root_hash.parse().with_context(|| {
            format!("parse collection hash from announce root_hash {:?}", pkg.root_hash)
        })?;
        let provider =
            EndpointId::from_bytes(&from).map_err(|e| anyhow!("invalid provider node id: {e}"))?;

        // Pin the downloaded collection under the same deterministic name the
        // provider used, so it survives GC until this receiver releases it. The
        // receiver releases the tag once it has acked ingestion, so a completed
        // transfer drops it promptly; a fetch or ingest that fails before the
        // ack leaves the tag pinned until the next process-startup sweep
        // (`start`'s `delete_all`) clears it as stale.
        let tag = package_tag(&pkg.package_id);
        blobs::fetch_collection_to_dir(
            &self.store,
            &self.endpoint,
            provider,
            root_hash,
            &tag,
            dest_dir,
        )
        .await?;

        // Best-effort completion progress (UI data; never blocks).
        let _ = self.event_tx.try_send(TransportEvent::FetchProgress {
            package_id: pkg.package_id.clone(),
            bytes_done: pkg.byte_size,
            bytes_total: pkg.byte_size,
        });
        tracing::debug!(from = %hex32(&from), package_id = %pkg.package_id.0, "iroh fetch complete");
        Ok(())
    }

    async fn serve(
        &self,
        pkg: &PackageAnnounce,
        src_dir: &Path,
        want: Option<&HashSet<String>>,
    ) -> Result<()> {
        let tag = package_tag(&pkg.package_id);
        // `None` → full package (pre-dedup). `Some(w)` → the negotiated subset:
        // only those payloads plus a manifest filtered to exactly them.
        let hash = match want {
            None => blobs::import_package_collection(&self.store, src_dir, &tag).await?,
            Some(w) => blobs::import_subset_collection(&self.store, src_dir, w, &tag).await?,
        };
        self.served
            .lock()
            .expect("served mutex poisoned")
            .insert(pkg.package_id.0.clone(), hash);
        tracing::debug!(
            package_id = %pkg.package_id.0,
            root_hash = %hash,
            path = %src_dir.display(),
            subset = want.is_some(),
            "iroh serving package"
        );
        Ok(())
    }

    async fn release(&self, package_id: &PackageId) -> Result<()> {
        self.served
            .lock()
            .expect("served mutex poisoned")
            .remove(&package_id.0);
        // `tags().delete` returns the removed count and does NOT error on a
        // missing tag — idempotency comes free.
        let removed = self
            .store
            .tags()
            .delete(package_tag(package_id))
            .await
            .map_err(|e| anyhow!("delete package tag: {e}"))?;
        tracing::debug!(package_id = %package_id.0, tags_removed = removed, "iroh released package");
        Ok(())
    }

    async fn ack(
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

    async fn negotiate_want(
        &self,
        to: NodeId,
        package_id: PackageId,
        offer: Vec<OfferEntry>,
        full_by_rel: HashMap<String, String>,
    ) -> Result<HashSet<String>> {
        // Round 1: Offer → Want. A non-Want reply (or any transport error) is a
        // protocol failure → Err → the engine falls back to a full announce.
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

        // Round 2: FullHashes → Want (still-wanted after full-hash disambiguation).
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

    async fn events(&self) -> mpsc::Receiver<TransportEvent> {
        let mut guard = self.event_rx.lock().expect("event_rx mutex poisoned");
        match guard.take() {
            Some(rx) => rx,
            None => {
                // Single-consumer: later calls get an already-closed receiver.
                let (_tx, rx) = mpsc::channel(1);
                rx
            }
        }
    }
}

/// The `athenaeum/sync/1` protocol handler: reads postcard [`Msg`]s off inbound
/// bidirectional streams and either republishes them as in-process
/// [`TransportEvent`]s (`Announce`/`Ack`, acked with a byte so the sender can
/// close cleanly) or answers them directly via the injected [`DedupResponder`]
/// (`Offer`/`FullHashes`, replied to with a real [`Msg::Want`]).
#[derive(Clone)]
struct SyncControlProtocol {
    event_tx: mpsc::Sender<TransportEvent>,
    /// Answers the dedup handshake. `None` on a send-only endpoint or a peer
    /// with no catalog wired — in which case offers are answered want-all.
    responder: Option<Arc<dyn DedupResponder>>,
    /// Connection-level authorization gate (slice 4). Checked once, at the top of
    /// `accept`, before any `Msg` is decoded. Absent ⇒ admit (accept-all).
    gate: SharedConnectGate,
}

// `dyn DedupResponder` isn't `Debug`, so the derive can't apply; the iroh
// `ProtocolHandler` bound requires `Debug`, so hand-roll a minimal one.
impl std::fmt::Debug for SyncControlProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncControlProtocol")
            .field("has_responder", &self.responder.is_some())
            .finish()
    }
}

impl ProtocolHandler for SyncControlProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        let from: NodeId = *connection.remote_id().as_bytes();
        // Connection-level authorization (slice 4): reject an ungated peer before
        // decoding a single `Msg`, so it gets no control dispatch at all.
        if !connect_gate_admits(&self.gate, &from) {
            tracing::warn!(from = %hex32(&from), "connection refused by connect gate");
            connection.close(0u32.into(), b"unauthorized");
            return Ok(());
        }
        spawn_conn_path_diagnostics(&connection, "incoming");
        loop {
            // One control message per bidi stream. `accept_bi` errors when the
            // peer closes the connection — the normal end of the loop.
            let (mut tx, mut rx) = match connection.accept_bi().await {
                Ok(stream) => stream,
                Err(_) => break,
            };
            let bytes = match rx.read_to_end(MAX_CONTROL_BYTES).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::warn!(from = %hex32(&from), error = %e, "read control stream failed");
                    break;
                }
            };
            let msg = match Msg::decode(&bytes) {
                Ok(msg) => msg,
                Err(e) => {
                    tracing::warn!(from = %hex32(&from), error = %e, "decode control message failed");
                    break;
                }
            };
            let event = match msg {
                Msg::Announce(announce) => TransportEvent::AnnounceReceived { from, announce },
                Msg::Ack {
                    package_id,
                    receipts,
                } => TransportEvent::AckReceived {
                    from,
                    package_id,
                    receipts,
                },
                // Collab exchange (slice 4): forward the project advertisement /
                // pull request as an in-process event, then the b"1" delivery ack
                // — the same deliver-then-ack shape as Announce/Ack.
                Msg::ProjectAnnounce {
                    project_id,
                    package_id,
                    announce,
                } => TransportEvent::ProjectAnnounceReceived {
                    from,
                    project_id,
                    package_id,
                    announce,
                },
                Msg::ProjectRequest {
                    project_id,
                    package_id,
                } => TransportEvent::ProjectRequestReceived {
                    from,
                    project_id,
                    package_id,
                },
                // Dedup handshake round 1: answer the offer with a real Want
                // reply (not the b"1" delivery ack) driven by the responder. No
                // responder → want-all, so a responder-less full peer still
                // receives everything (nothing silently withheld).
                Msg::Offer {
                    package_id,
                    entries,
                } => {
                    // `want_for_offer` does blocking catalog DB I/O — run it off
                    // the async worker so it can't stall the accept loop / other
                    // connections. A `spawn_blocking` join failure (the responder
                    // never panics) falls back to the safe direction: want
                    // everything, no candidates — matching the None branch.
                    let (want, candidates) = match self.responder.clone() {
                        Some(r) => {
                            let entries2 = entries.clone();
                            tokio::task::spawn_blocking(move || r.want_for_offer(&entries2))
                                .await
                                .unwrap_or_else(|_| {
                                    (
                                        entries.iter().map(|e| e.rel_path.clone()).collect(),
                                        Vec::new(),
                                    )
                                })
                        }
                        None => (
                            entries.iter().map(|e| e.rel_path.clone()).collect(),
                            Vec::new(),
                        ),
                    };
                    write_reply(
                        &mut tx,
                        &Msg::Want {
                            package_id,
                            want,
                            candidates,
                        },
                        &from,
                    )
                    .await;
                    continue;
                }
                // Dedup handshake round 2: confirm the candidates' full hashes
                // and reply with the still-wanted subset. No responder → keep
                // them all wanted (safe direction).
                Msg::FullHashes {
                    package_id,
                    entries,
                } => {
                    // `confirm_full_hashes` streams and hashes every candidate
                    // file from disk (potentially many GB on a re-send) — move it
                    // off the async worker. A join failure resolves to the safe
                    // direction: keep every candidate wanted (the None branch).
                    let still = match self.responder.clone() {
                        Some(r) => {
                            let entries2 = entries.clone();
                            tokio::task::spawn_blocking(move || r.confirm_full_hashes(&entries2))
                                .await
                                .unwrap_or_else(|_| {
                                    entries.iter().map(|e| e.rel_path.clone()).collect()
                                })
                        }
                        None => entries.iter().map(|e| e.rel_path.clone()).collect(),
                    };
                    write_reply(
                        &mut tx,
                        &Msg::Want {
                            package_id,
                            want: still,
                            candidates: Vec::new(),
                        },
                        &from,
                    )
                    .await;
                    continue;
                }
                // A well-behaved peer never sends us a Want as a request — it is
                // the reply to our own Offer/FullHashes on the sender side.
                // Treat an inbound one as a protocol error: skip it (the stream
                // closes with no reply, so a confused sender's read errors and it
                // falls back to a full announce).
                Msg::Want { .. } => {
                    tracing::warn!(from = %hex32(&from), "unexpected inbound Want on control accept; ignoring");
                    continue;
                }
            };
            // Deliver in-process, then ack. A closed consumer means the engine is
            // gone — stop accepting and leave the ack unsent so the sender retries.
            if self.event_tx.send(event).await.is_err() {
                tracing::debug!(from = %hex32(&from), "control event consumer gone; closing");
                break;
            }
            let _ = tx.write_all(b"1").await;
            let _ = tx.finish();
        }
        Ok(())
    }
}

/// A thin gating wrapper around the iroh-blobs [`ProtocolHandler`] (collab
/// exchange, slice 4). It evaluates the shared connect gate against the dialing
/// peer's node id and only then delegates to the inner `iroh_blobs` handler, so
/// an ungated peer never receives a single blob byte (finding F5 hardening).
/// With no gate installed it delegates unconditionally — today's behavior.
struct GatedBlobs {
    inner: BlobsProtocol,
    gate: SharedConnectGate,
}

// `SharedConnectGate` wraps a boxed closure (not `Debug`), so the `ProtocolHandler`
// `Debug` bound can't be derived — hand-roll a minimal impl.
impl std::fmt::Debug for GatedBlobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatedBlobs").finish()
    }
}

impl ProtocolHandler for GatedBlobs {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        let from: NodeId = *connection.remote_id().as_bytes();
        if !connect_gate_admits(&self.gate, &from) {
            tracing::warn!(from = %hex32(&from), "connection refused by connect gate");
            connection.close(0u32.into(), b"unauthorized");
            return Ok(());
        }
        // A blob download connection lives for the whole transfer — the longest
        // window this transport holds a `Connection` handle, so its path watcher
        // is the most likely to observe a mid-transfer relay→direct upgrade.
        spawn_conn_path_diagnostics(&connection, "incoming");
        <BlobsProtocol as ProtocolHandler>::accept(&self.inner, connection).await
    }

    // Forward the router-shutdown hook to the inner handler so the blobs store is
    // still flushed on `Router::shutdown` — `BlobsProtocol` overrides `shutdown`,
    // and the default (no-op) would otherwise silently drop that flush.
    async fn shutdown(&self) {
        <BlobsProtocol as ProtocolHandler>::shutdown(&self.inner).await
    }
}

/// Encode a reply [`Msg`] and write it back on an accept-side bidi stream,
/// finishing the send half. Best-effort: a write/finish failure only means the
/// requester's read errors — which correctly drives its negotiation fallback —
/// and an encode failure (never expected for a well-formed `Want`) is logged.
async fn write_reply(tx: &mut iroh::endpoint::SendStream, reply: &Msg, from: &NodeId) {
    match reply.encode() {
        Ok(bytes) => {
            let _ = tx.write_all(&bytes).await;
            let _ = tx.finish();
        }
        Err(e) => tracing::warn!(from = %hex32(from), error = %e, "encode dedup reply failed"),
    }
}

/// Classify a live connection's transport path from its open-path snapshot,
/// returning `(conn_type, addr)` for logging:
///
/// - `conn_type` — `"direct"` (only IP paths open), `"relay"` (only relay
///   paths), `"mixed"` (both open — typically mid hole-punch), or `"pending"`
///   (no path recorded yet).
/// - `addr` — the *selected* transmission path's remote transport address,
///   rendered with its `ip:<socket>` / `relay:<url>` prefix; falls back to any
///   open path, or `None` when the snapshot is empty.
fn describe_conn_path(conn: &Connection) -> (&'static str, Option<String>) {
    let paths = conn.paths();
    let mut has_ip = false;
    let mut has_relay = false;
    let mut selected: Option<String> = None;
    let mut any: Option<String> = None;
    for p in paths.iter() {
        has_ip |= p.is_ip();
        has_relay |= p.is_relay();
        if p.is_selected() {
            selected = Some(p.remote_addr().to_string());
        } else if any.is_none() {
            any = Some(p.remote_addr().to_string());
        }
    }
    let conn_type = match (has_ip, has_relay) {
        (true, true) => "mixed",
        (true, false) => "direct",
        (false, true) => "relay",
        (false, false) => "pending",
    };
    (conn_type, selected.or(any))
}

/// Log a connection's established transport path (`info!`) and spawn a
/// lightweight watcher that logs any later *path-type* change — a relay→direct
/// hole-punch upgrade is the single most diagnostic event for NAT work.
///
/// The watcher consumes the connection's `'static` [`PathEventStream`], which
/// ends when the connection closes (the endpoint drops that connection's
/// per-connection path-state sender). So the task's lifetime is tied to the
/// connection's exactly the way iroh ties its own per-connection state — it can
/// never outlive the connection and never leaks. Fires on a *selection change*
/// only, never per-packet/per-poll.
fn spawn_conn_path_diagnostics(conn: &Connection, direction: &'static str) {
    let peer = conn.remote_id().fmt_short();
    let (conn_type, addr) = describe_conn_path(conn);
    tracing::info!(
        peer = %peer,
        direction,
        conn_type,
        addr = addr.as_deref().unwrap_or("none"),
        "connection path established"
    );

    let mut events = conn.path_events();
    let mut last_type = conn_type;
    tokio::spawn(async move {
        use n0_future::StreamExt as _;
        while let Some(event) = events.next().await {
            // Only a `Selected` event moves the transmission path; classify the
            // newly-selected remote address and log iff the direct/relay type
            // flips (e.g. relay → direct after a successful hole-punch).
            if let iroh::endpoint::PathEvent::Selected { remote_addr, .. } = event {
                let new_type = if remote_addr.is_relay() {
                    "relay"
                } else if remote_addr.is_ip() {
                    "direct"
                } else {
                    "other"
                };
                if new_type != last_type {
                    tracing::info!(
                        peer = %peer,
                        direction,
                        conn_type = new_type,
                        addr = %remote_addr,
                        "connection path changed"
                    );
                    last_type = new_type;
                }
            }
        }
    });
}

/// Generate a fresh random 32-byte secret key (for ephemeral endpoints / tests).
pub fn random_secret() -> [u8; 32] {
    SecretKey::generate().to_bytes()
}

/// Lowercase-hex rendering of a node id for log fields (matches the loopback
/// mock's field format).
fn hex32(node_id: &NodeId) -> String {
    node_id.iter().map(|b| format!("{b:02x}")).collect()
}
