//! Wire types for the sharing transport.
//!
//! These are the payloads a [`SharingTransport`](crate::sharing::SharingTransport)
//! moves between peers. They are transport-agnostic: the same types flow over
//! the in-process [`LoopbackTransport`](crate::sharing::loopback::LoopbackTransport)
//! mock and (later) a real iroh transport. No catalog/DB/engine concepts leak
//! in here — this layer only knows about announcements, blobs, and receipts.

use serde::{Deserialize, Serialize};

/// A peer's stable identity: an ed25519 public key, byte-identical to the iroh
/// node id in the real transport.
pub type NodeId = [u8; 32];

/// UUID-v4 string identifying one shareable package (a set of frames + manifest).
///
/// Carries `Serialize`/`Deserialize`/`Clone` because it is embedded in the
/// serializable [`PackageAnnounce`]; the extra `Debug`/`Eq`/`Hash` derives make
/// it usable as a map key and in assertions.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PackageId(pub String);

/// The metadata a provider broadcasts to advertise a fetchable package.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PackageAnnounce {
    pub package_id: PackageId,
    pub root_hash: String,
    pub byte_size: u64,
    pub frame_count: u32,
}

/// The receiver's verdict on one frame, returned to the provider in an ack.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FrameReceipt {
    pub frame_uuid: String,
    pub xxh3: String,
    pub outcome: ReceiptOutcome,
}

/// What happened to a frame on the receiving side.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ReceiptOutcome {
    Ingested,
    Duplicate,
    Rejected(String),
    /// The receiver deliberately declined this frame — a first-class "no", NOT a
    /// transient rejection. Unlike [`Rejected`](Self::Rejected) (which keeps the
    /// package in flight for redelivery), a package whose ack is entirely
    /// `Cancelled` is terminal-by-receiver: the sender stops retrying and marks
    /// the outbound row cancelled. This is the delivery-forever cycle's single
    /// new wire value (spec §5); an old peer cannot deserialize it — accepted, no
    /// compat shim. Produced by the receiver-cancel flow (a later task).
    Cancelled,
}

/// Metadata returned when an endpoint comes online.
///
/// `pairing_ticket` is an opaque, out-of-band-shareable string; in the real
/// transport it is an iroh ticket, in the mock it is a base64 of the node id.
#[derive(Clone, Debug)]
pub struct StartInfo {
    pub node_id: NodeId,
    pub pairing_ticket: String,
}

/// Asynchronous notifications delivered on an endpoint's [`events`] stream.
///
/// Delivered in-process only (never serialized), so this type intentionally
/// carries no serde derives.
///
/// [`events`]: crate::sharing::SharingTransport::events
#[derive(Clone, Debug)]
pub enum TransportEvent {
    /// A peer announced a package to us.
    AnnounceReceived {
        from: NodeId,
        announce: PackageAnnounce,
    },
    /// A peer acknowledged a package we sent, returning per-frame receipts.
    AckReceived {
        from: NodeId,
        package_id: PackageId,
        receipts: Vec<FrameReceipt>,
    },
    /// Progress of a fetch this endpoint is performing.
    FetchProgress {
        package_id: PackageId,
        bytes_done: u64,
        bytes_total: u64,
    },
    /// A peer advertised a PROJECT package (collab exchange, slice 4). Carries the
    /// HUB package uuid (`package_id`) alongside the engine's wire `announce`
    /// (whose own `package_id` is the ack-correlation id).
    ProjectAnnounceReceived {
        from: NodeId,
        project_id: String,
        package_id: String,
        announce: PackageAnnounce,
    },
    /// A receive-role member asked us (a holder) to serve a project package.
    ProjectRequestReceived {
        from: NodeId,
        project_id: String,
        package_id: String,
    },
}
