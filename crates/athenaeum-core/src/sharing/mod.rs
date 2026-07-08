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
    async fn serve(&self, pkg: &PackageAnnounce, src_dir: &Path) -> anyhow::Result<()>;

    /// Acknowledge a received package to peer `to`, returning per-frame receipts.
    async fn ack(
        &self,
        to: NodeId,
        package_id: &PackageId,
        receipts: Vec<FrameReceipt>,
    ) -> anyhow::Result<()>;

    /// Drop the local payload data for `package_id` — the package reached a
    /// terminal state (confirmed / failed / cancelled on the sender; acked on
    /// the receiver) and its blobs must not outlive it. Idempotent: releasing
    /// an unknown or already-released package is Ok(()). Never fails the
    /// caller's state transition — callers log-and-continue on Err.
    async fn release(&self, package_id: &PackageId) -> anyhow::Result<()>;

    /// Hand out the receiving half of this endpoint's [`TransportEvent`] stream.
    ///
    /// Single-consumer: the receiver is returned on the first call; subsequent
    /// calls yield an already-closed receiver (`recv()` returns `None`
    /// immediately). Callers (the engine) must take it exactly once.
    async fn events(&self) -> mpsc::Receiver<TransportEvent>;
}
