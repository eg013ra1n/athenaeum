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

use std::path::Path;

use async_trait::async_trait;
use tokio::sync::mpsc;

pub mod iroh;
pub mod loopback;
pub mod types;

#[cfg(test)]
mod tests;

pub use types::{
    FrameReceipt, NodeId, PackageAnnounce, PackageId, ReceiptOutcome, StartInfo, TransportEvent,
};

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
    async fn announce(&self, to: NodeId, a: &PackageAnnounce) -> anyhow::Result<()>;

    /// Pull a package (manifest + blobs) from `from` into `dest_dir`.
    ///
    /// Verified and resumable in the real transport; the mock re-copies the
    /// served directory on each call (idempotent overwrite).
    async fn fetch(&self, from: NodeId, pkg: &PackageAnnounce, dest_dir: &Path)
        -> anyhow::Result<()>;

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
