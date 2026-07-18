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
    /// Upload progress for a package WE are serving (Task 13). Emitted locally —
    /// by the iroh provider-upload-events consumer as a peer pulls our collection,
    /// or as one synthetic tick from the loopback mock — carrying the cumulative
    /// bytes sent for `package_id`. Routed to the sender engine, which turns it
    /// into a send-side `sync-progress` `transferring` tick with byte figures.
    /// Unlike the other variants this originates on OUR endpoint, not from a
    /// peer's control message, so it carries no `from`.
    ServeProgress {
        package_id: PackageId,
        bytes_sent: u64,
    },
    /// A package WE are serving has finished uploading to a peer (Task 2.1):
    /// the iroh provider reported the terminal `Completed` of a
    /// **payload-carrying** GET request (one whose ranges reach a collection
    /// file entry, hash-seq offset ≥ 2 — never a phase-1 root+meta pull or a
    /// manifest probe). Like [`ServeProgress`](Self::ServeProgress) this
    /// originates LOCALLY — on OUR endpoint, from the provider-upload-events
    /// consumer, or as one synthetic signal from the loopback mock — never from
    /// a peer control message, so it carries no `from`. Routed to the sender
    /// engine, which turns it into an "uploaded — awaiting confirmation" tick
    /// (`sync-progress` `stage = "uploaded"`) with NO store write and NO state
    /// transition: the receiver's ack stays the only delivery truth. A later
    /// payload request (a resume) produces fresh `ServeProgress` ticks that flip
    /// the stage back to `transferring`, self-correcting.
    ServeComplete { package_id: PackageId },
}

/// Live progress of a fetch, delivered on the [`FetchSink`] callback threaded
/// into [`SharingTransport::fetch`], NOT on the shared [`TransportEvent`] stream.
///
/// The receiver loop awaits `fetch` inline and does not drain its event channel
/// during a fetch, so routing per-file/batch progress through that channel would
/// risk backpressure or deadlock. This callback is the seam instead: the caller
/// (Task 11) turns each event into UI progress. Progress is UI data, never a log.
///
/// [`FetchSink`]: crate::sharing::FetchSink
/// [`SharingTransport::fetch`]: crate::sharing::SharingTransport::fetch
#[derive(Clone, Debug)]
pub enum FetchEvent {
    /// Aggregate progress across the whole collection download. `bytes_done` is
    /// cumulative request bytes (including any already-present locally), clamped
    /// to `bytes_total` (the announce's `byte_size`).
    Batch { bytes_done: u64, bytes_total: u64 },
    /// Per-file progress for one collection entry, keyed by its `name`
    /// (forward-slash `rel_path`). Ends with `bytes_done == bytes_total`.
    File {
        name: String,
        bytes_done: u64,
        bytes_total: u64,
    },
}
