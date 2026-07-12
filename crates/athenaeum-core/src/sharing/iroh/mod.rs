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
        let lookup = iroh::address_lookup::memory::MemoryLookup::new();

        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(secret_key)
            .relay_mode(relay_mode)
            .address_lookup(lookup.clone())
            .bind()
            .await
            .context("bind iroh endpoint")?;

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
        // Finding F5 (accepted residual, LOW): the blobs provider serves any
        // stored blob to any node that requests it BY HASH. This is not the
        // exploitable boundary — the real attack (unauthorized *ingestion*) is
        // blocked receiver-side by the H1 peer-authorization gate in
        // `sync::receiver`. Pulling a blob additionally requires the collection
        // `root_hash`, an unguessable BLAKE3 digest transmitted only to the
        // authorized peer over encrypted QUIC, and served content is released
        // promptly after ack. iroh-blobs 0.103 does expose a connect-time
        // `ConnectMode::Intercept` reject hook; wiring a dynamic allow-list into
        // it is a possible future hardening, deliberately not taken here to keep
        // the transport free of authorization state (which lives in the host).
        let blobs = BlobsProtocol::new(&store, None);
        let control = SyncControlProtocol {
            event_tx: event_tx.clone(),
            responder,
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
        })
    }

    /// This endpoint's node id, available before [`start`](SharingTransport::start).
    pub fn node_id(&self) -> NodeId {
        self.node_id
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
        if self.uses_relay {
            let _ = tokio::time::timeout(ONLINE_TIMEOUT, self.endpoint.online()).await;
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
                // Dedup handshake round 1: answer the offer with a real Want
                // reply (not the b"1" delivery ack) driven by the responder. No
                // responder → want-all, so a responder-less full peer still
                // receives everything (nothing silently withheld).
                Msg::Offer {
                    package_id,
                    entries,
                } => {
                    let (want, candidates) = match &self.responder {
                        Some(r) => r.want_for_offer(&entries),
                        None => (entries.iter().map(|e| e.rel_path.clone()).collect(), Vec::new()),
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
                    let still = match &self.responder {
                        Some(r) => r.confirm_full_hashes(&entries),
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

/// Generate a fresh random 32-byte secret key (for ephemeral endpoints / tests).
pub fn random_secret() -> [u8; 32] {
    SecretKey::generate().to_bytes()
}

/// Lowercase-hex rendering of a node id for log fields (matches the loopback
/// mock's field format).
fn hex32(node_id: &NodeId) -> String {
    node_id.iter().map(|b| format!("{b:02x}")).collect()
}
