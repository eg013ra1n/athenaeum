//! Transport-agnostic peer-to-peer sharing layer (Stage I personal sync).
//!
//! This module defines [`SharingTransport`] — the seam between the sync engine
//! (task A4) and whatever moves bytes between peers. It has exactly two
//! implementors: a real iroh transport (task A5) and the in-process
//! [`LoopbackTransport`] mock in [`loopback`], used to exercise the engine
//! (announce/fetch/serve/ack/events, plus fault injection) without a network.
//!
//! Scope discipline: this layer knows only about announcements, blobs, and
//! receipts. Catalog, DB, and integration-engine logic live above it in the
//! sync engine; nothing here touches the render/solver features, so the module
//! compiles in the headless (`--no-default-features`) build.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

pub mod iroh;
pub mod loopback;
pub mod types;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod wire_golden_tests;

pub use types::{
    AnnounceFileEntry, FetchEvent, FrameReceipt, NodeId, PackageAnnounce, PackageAnnounceV2,
    PackageAnnounceV3, PackageId, ReceiptOutcome, RevokeReason, StartInfo, TransportEvent,
};

/// A callback that receives live [`FetchEvent`]s while a [`fetch`] is in flight.
///
/// Threaded directly into [`fetch`] rather than routed through the endpoint's
/// [`TransportEvent`] stream: the receiver awaits `fetch` inline and does not
/// drain that channel mid-fetch, so a callback avoids the backpressure/deadlock
/// a shared channel would risk. `Arc` so a single sink can fan out to the batch
/// stream plus one observer task per collection entry.
///
/// [`fetch`]: SharingTransport::fetch
pub type FetchSink = Arc<dyn Fn(FetchEvent) + Send + Sync>;

/// A [`FetchSink`] that discards every event — for call sites that do not (yet)
/// surface fetch progress. Task 11 replaces these with a real sink.
pub fn noop_fetch_sink() -> FetchSink {
    Arc::new(|_| {})
}

/// A per-provider telemetry event from a multi-source (swarm) fetch —
/// [`fetch_collection_multi`](SharingTransport::fetch_collection_multi), D3 §3.1.3.
///
/// A swarm fetch runs one request per collection child across the whole provider
/// set, and the downloader's progress stream is the ONLY place that says which
/// provider a given child is being pulled from. The scalar [`fetch`] drops those
/// items on the floor; the swarm fetch routes them here, so a caller can journal
/// per-provider transitions and drive a live "downloading from N sources" figure.
///
/// Advisory telemetry, never control flow: a provider reported [`Failed`] is
/// retried against the next provider by the downloader's own failover, and a
/// caller that discards every event changes nothing about the transfer.
///
/// [`fetch`]: SharingTransport::fetch
/// [`Failed`]: ProviderEvent::Failed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderEvent {
    /// The downloader is about to ask this provider for one child request.
    /// Emitted once per (child, provider) attempt — *including* the attempt that
    /// then finds the child already complete locally — so `Trying` proves the
    /// provider entered the rotation, not that bytes moved.
    Trying(NodeId),
    /// This provider could not be dialed, or its transfer errored, for one child
    /// request. The downloader moves on to the next provider for that child and
    /// asks it only for the bytes still missing, so a `Failed` is a provider
    /// switch, NOT a failed fetch.
    Failed(NodeId),
}

/// A callback receiving live [`ProviderEvent`]s while a multi-source fetch is in
/// flight — the per-provider counterpart of [`FetchSink`], threaded the same way
/// and for the same reason (the caller awaits the fetch inline and drains no
/// channel meanwhile, so a callback avoids the backpressure a shared channel
/// would risk).
pub type ProviderTelemetrySink = Arc<dyn Fn(ProviderEvent) + Send + Sync>;

/// A [`ProviderTelemetrySink`] that discards every event — for call sites that
/// do not surface per-provider detail.
pub fn noop_provider_telemetry() -> ProviderTelemetrySink {
    Arc::new(|_| {})
}

/// A bidirectional peer-to-peer transport for sharing frame packages.
///
/// Implementors move [`PackageAnnounce`]s, package blobs, and [`FrameReceipt`]
/// acks between peers, surfacing inbound activity on a single [`events`] stream.
/// The trait deliberately holds no catalog/DB state — the sync engine composes
/// these primitives.
///
/// [`events`]: SharingTransport::events
#[async_trait]
pub trait SharingTransport: Send + Sync {
    /// Bring the endpoint online and return its identity + pairing ticket.
    async fn start(&self) -> anyhow::Result<StartInfo>;

    /// Broadcast a package announcement to peer `to`.
    ///
    /// The app sender emits only v3: implementors encode a `Msg::Announce3`
    /// carrying `a`'s fields plus `batch_name` (a human batch display name),
    /// `batch_uuid` (the durable per-transfer identity, spec §D2), and `files`
    /// (the full manifest). The engine passes the package-dir basename as
    /// `batch_uuid` — which per spec D1/B3 IS the final batch identity for an
    /// outbound transfer, so it is the real value, not a placeholder. The receive
    /// side accepts all three announce versions; a legacy v1 (`Msg::Announce`,
    /// extras `None`) / v2 (`Msg::Announce2`) announce falls back to the wire
    /// `package_id` as its `batch_uuid`.
    async fn announce(
        &self,
        to: NodeId,
        a: &PackageAnnounce,
        batch_name: &str,
        batch_uuid: &str,
        files: &[AnnounceFileEntry],
    ) -> anyhow::Result<()>;

    /// Revoke an outstanding announce to peer `to` (spec §D2): tell the receiver
    /// to abort the pending / in-flight transfer for `package_id` (the current
    /// attempt's wire id) because it was `reason` (cancelled / superseded /
    /// failed). A one-shot, best-effort control message — the caller (B3)
    /// log-and-continues on `Err`; it never fails a sender state transition. No
    /// caller wires this yet.
    ///
    /// The default bails (like the project methods): a transport without revoke
    /// support fails loudly rather than silently swallowing a revoke. The two real
    /// iroh transports and the in-process loopback mock override it.
    async fn revoke(
        &self,
        to: NodeId,
        package_id: &PackageId,
        reason: crate::sharing::types::RevokeReason,
    ) -> anyhow::Result<()> {
        let _ = (to, package_id, reason);
        anyhow::bail!("transport does not support revoke")
    }

    /// Pull a package (manifest + blobs) from `from` into `dest_dir`.
    ///
    /// Verified and resumable in the real transport; the mock re-copies the
    /// served directory on each call (idempotent overwrite). `sink` receives live
    /// [`FetchEvent`]s (per-file + batch) while the transfer is in flight; pass
    /// [`noop_fetch_sink`] when progress is not consumed.
    async fn fetch(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
        sink: FetchSink,
    ) -> anyhow::Result<()>;

    /// Pull one collection, identified by its `root_hash`, from MANY `providers`
    /// at once into `dest_dir` — the swarm fetch (D3 §3.1).
    ///
    /// Unlike [`fetch`](Self::fetch), which pulls a package announced by ONE
    /// peer, this takes the holder set as given (the hub's holder list, minus
    /// self) and lets the blob layer split the collection across it: one request
    /// per child blob, each with the full provider set behind it, so a holder
    /// that dies mid-transfer costs its children a provider switch with
    /// byte-level resume instead of costing the fetch. `providers` is a
    /// snapshot: no live re-discovery mid-fetch.
    ///
    /// `sink` receives the same per-file/batch [`FetchEvent`]s as `fetch`;
    /// `telemetry` receives the per-provider [`ProviderEvent`]s only a
    /// multi-source fetch can produce (pass [`noop_provider_telemetry`] when
    /// they are not consumed).
    ///
    /// The default bails: the in-process loopback mock and Perseus have no
    /// swarm to fetch from, and a transport without multi-source support must
    /// fail loudly rather than silently degrade to a single provider — the
    /// caller's fallback is an explicit code path, not an accident.
    async fn fetch_collection_multi(
        &self,
        providers: Vec<NodeId>,
        root_hash: &str,
        byte_size: u64,
        dest_dir: &Path,
        sink: FetchSink,
        telemetry: ProviderTelemetrySink,
    ) -> anyhow::Result<()> {
        let _ = (providers, root_hash, byte_size, dest_dir, sink, telemetry);
        anyhow::bail!("transport does not support multi-source fetch")
    }

    /// Pull ONLY the package manifest (`manifest.ndjson`) from `from` into
    /// `dest_dir`, returning its path. The real transport fetches just the
    /// hash-sequence + collection meta and the single named manifest blob (not the
    /// payload frames); the mock copies the manifest file from the served dir.
    /// Used by the receiver-cancel flow (a later task) to inspect a package's
    /// frames without downloading them.
    async fn fetch_manifest(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
    ) -> anyhow::Result<PathBuf>;

    /// Register the local package directory that peers may fetch from
    /// (provider side).
    ///
    /// `want` narrows what is served to the frames the peer still wants after
    /// the dedup handshake: `None` serves the full package (the pre-dedup
    /// behavior); `Some(rel_paths)` serves only those payloads plus a manifest
    /// filtered to exactly them, so the receiver ingests the negotiated subset
    /// and never sees the frames it already had. `want` must be non-empty — an
    /// all-duplicate package is dropped before serve, not served empty.
    async fn serve(
        &self,
        pkg: &PackageAnnounce,
        src_dir: &Path,
        want: Option<&std::collections::HashSet<String>>,
    ) -> anyhow::Result<()>;

    /// Acknowledge a received package to peer `to`, returning per-frame receipts.
    async fn ack(
        &self,
        to: NodeId,
        package_id: &PackageId,
        receipts: Vec<FrameReceipt>,
    ) -> anyhow::Result<()>;

    /// Best-effort pre-Announce dedup negotiation: offer peer `to` the frames of
    /// `package_id` (keyed `rel_path` + sampling hash), returning the `rel_path`s
    /// the peer still wants after the offer/want[/full-hashes] handshake.
    /// `full_by_rel` maps each offered `rel_path` to its full-file xxh3, used to
    /// disambiguate sampling-hash collisions in the second round.
    ///
    /// `Err` is the contract for "fall back": any transport/decode/timeout error
    /// (or an old peer that can't answer the handshake) propagates, and the
    /// caller must announce the full package instead. The default bails, so a
    /// transport that hasn't implemented the handshake degrades to that fallback.
    async fn negotiate_want(
        &self,
        to: NodeId,
        package_id: PackageId,
        offer: Vec<crate::sharing::iroh::proto::OfferEntry>,
        full_by_rel: std::collections::HashMap<String, String>,
    ) -> anyhow::Result<std::collections::HashSet<String>> {
        let _ = (to, package_id, offer, full_by_rel);
        anyhow::bail!("negotiate_want unsupported by this transport")
    }

    /// Advertise a collaboration PROJECT package to peer `to` (slice 4). Mirrors
    /// [`announce`](Self::announce) but carries the HUB package uuid + project id
    /// on the wire so the receiver can correlate it to its hub project row; the
    /// engine-minted `announce.package_id` still correlates the eventual ack. The
    /// default bails, so a transport without project support fails loudly rather
    /// than silently degrading to a personal-sync announce.
    async fn announce_project(
        &self,
        to: NodeId,
        project_id: &str,
        package_id: &str,
        announce: &PackageAnnounce,
    ) -> anyhow::Result<()> {
        let _ = (to, project_id, package_id, announce);
        anyhow::bail!("transport does not support project exchange")
    }

    /// Ask holder `to` to serve a project package identified by its HUB uuid
    /// (`package_id` — the holder's row key), a receive-role member's pull
    /// request (slice 4). The default bails on a transport without project
    /// support.
    async fn request_project(
        &self,
        to: NodeId,
        project_id: &str,
        package_id: &str,
    ) -> anyhow::Result<()> {
        let _ = (to, project_id, package_id);
        anyhow::bail!("transport does not support project exchange")
    }

    /// Drop the local payload data for `package_id` — the package reached a
    /// terminal state (confirmed / failed / cancelled on the sender; acked on
    /// the receiver) and its blobs must not outlive it. Idempotent: releasing
    /// an unknown or already-released package is Ok(()). Never fails the
    /// caller's state transition — callers log-and-continue on Err.
    async fn release(&self, package_id: &PackageId) -> anyhow::Result<()>;

    /// List the wire [`PackageId`]s of the receiver-side **in-flight download
    /// tags** currently GC-protecting a partial collection in this transport's
    /// blob store (Transfers Batch Model §D4, B7 orphan reclaim).
    ///
    /// An aborted/killed fetch deliberately keeps its `in-flight/…` tag so a
    /// later resume can reuse the verified partial bytes (B0 findings Q4); the
    /// receiver's startup sweep and the "clean up finished transfers" action
    /// call this to find those tags, cross-check them against the durable
    /// `sync_inbound` rows, and [`release`](Self::release) the ones whose
    /// transfer is no longer non-terminal so GC can reclaim the blobs.
    ///
    /// **Namespace contract (B7):** the implementation enumerates ONLY the
    /// transfer-owned in-flight namespace (`in-flight/recv/pkg/<id>`) and returns
    /// exactly those wire ids — a permanent `<role>/pkg/…` tag, a future
    /// `project/…` seeding tag, or any foreign tag is never returned, so a caller
    /// that releases every id this yields can never touch a tag outside the
    /// transfer machinery. The default returns empty (the in-process loopback
    /// mock and any store-less transport carry no such tags unless a test seeds
    /// them).
    async fn list_in_flight_tags(&self) -> anyhow::Result<Vec<PackageId>> {
        Ok(Vec::new())
    }

    /// Tell `to` that we are online and listening (D1 presence beacon).
    ///
    /// Sent by a RECEIVER when it becomes reachable, so a sender parked on a
    /// retry resumes at once instead of waiting out its schedule. Best-effort by
    /// contract: the caller logs a failure and moves on, because the sender's
    /// retry schedule is the fallback for every peer that never beacons (an older
    /// build, or a route that broke without either side restarting).
    ///
    /// The default is a no-op so a transport with no control channel — the
    /// in-process loopback mock, a test double — need not implement it.
    async fn send_presence(&self, _to: NodeId) -> anyhow::Result<()> {
        Ok(())
    }

    /// Attach a best-effort dial hint for an inbound peer `from` right before we
    /// pull its blobs (finding H1 / I2, iroh hardening T7).
    ///
    /// A peer that announced to us dialed IN; the blob pull dials back OUT, and
    /// with `presets::Minimal` (no discovery) the downloader needs an address for
    /// `from`. The real node override merges a relay hint — our own relay set,
    /// which same-account devices share — into the address lookup the downloader
    /// dials through. Merge-only: it never downgrades a richer address the node
    /// already knows, and an empty relay set is a no-op (exactly the pre-T7
    /// behavior). `from` has already cleared the receiver's authorization /
    /// project-membership gate by the time this is called, so it is a known peer.
    /// The default is a no-op — the in-process loopback transport dials
    /// in-process and needs no hint.
    fn add_peer_dial_hint(&self, from: NodeId) {
        let _ = from;
    }

    /// Register / refresh a peer's dialable [`EndpointAddr`] (retry re-resolution,
    /// iroh hardening T8). The sync engine calls this from its timeout path when
    /// its address refresher yields a fresh address for the peer whose earlier
    /// attempts timed out — a relay-map change or the peer moving relays can strand
    /// a cached address, so re-registering the peer's CURRENT address lets the next
    /// re-attempt dial the live path instead of the dead one. The default is a
    /// no-op (the in-process loopback transport dials in-process); the shared-node
    /// role handle merges it into the node's peer book + address lookup.
    fn add_peer_addr(&self, addr: ::iroh::EndpointAddr) {
        let _ = addr;
    }

    /// Invalidate a peer's cached address-lookup entry before a fresh address
    /// replaces it (iroh hardening T8 / Task 3.6). The sync engine's retry path
    /// calls this — and ONLY when its refresher already yielded a fresh address —
    /// when the last dial failed in a way that implicates stale peer addressing
    /// (`no_route` / `timeout` / `relay_unreachable`). Because a
    /// [`MemoryLookup`](iroh::address_lookup::memory::MemoryLookup) merge
    /// (`add_endpoint_info`) can never *remove* a now-dead relay URL, the engine
    /// removes the stale entry first so the immediately following
    /// [`add_peer_addr`](Self::add_peer_addr) fully REPLACES it instead of merging
    /// fresh hints on top of a dead relay. The default is a no-op (the in-process
    /// loopback transport dials in-process); the shared-node role handle removes
    /// the peer from the node's address lookup. Never called without a replacement
    /// in hand, so last-known addressing is never dropped on a `None` refresh.
    fn remove_peer_addr(&self, peer: NodeId) {
        let _ = peer;
    }

    /// Hand out the receiving half of this endpoint's [`TransportEvent`] stream.
    ///
    /// Single-consumer: the receiver is returned on the first call; subsequent
    /// calls yield an already-closed receiver (`recv()` returns `None`
    /// immediately). Callers (the engine) must take it exactly once.
    async fn events(&self) -> mpsc::Receiver<TransportEvent>;

    /// Gracefully tear down whatever this transport owns (iroh hardening I1).
    ///
    /// Production transports are `Arc`-wrapped, so the drop-only teardown path
    /// aborts the router mid-poll and skips `endpoint.close()` — peers then see
    /// QUIC resets and a fast-restarting process leaves its old relay
    /// registration lingering. A host that wants a clean close awaits this on
    /// shutdown (receiver stop, web/Tauri exit, Perseus `Agent::shutdown`).
    ///
    /// The default is a no-op: the in-process [`LoopbackTransport`] and the
    /// legacy owned-`IrohTransport::shutdown` path have nothing extra to do here.
    /// The [`SharedIrohNode`](crate::sharing::iroh::node::SharedIrohNode) role
    /// handles route it to the node's single bounded teardown. Idempotent.
    async fn shutdown(&self) {}
}
