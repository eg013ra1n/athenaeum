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

/// One manifest entry advertised in a v2 announce ([`PackageAnnounceV2`]).
///
/// Lets the receiver learn a package's file contents at announce time — before
/// any fetch — so a later task can create per-file rows from the manifest. Byte
/// image is pinned by `sharing::wire_golden_tests`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnounceFileEntry {
    pub rel_path: String,
    pub byte_size: u64,
    pub frame_uuid: String,
}

/// V2 package announce: the [`PackageAnnounce`] fields plus a human batch display
/// name and the full file manifest.
///
/// A separate type (not new fields on [`PackageAnnounce`], whose positional
/// postcard bytes are frozen) carried by the appended `Msg::Announce2` wire
/// variant. `Msg::Announce` (v1) stays byte-identical for old peers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageAnnounceV2 {
    pub package_id: PackageId,
    pub root_hash: String,
    pub byte_size: u64,
    pub frame_count: u32,
    pub batch_name: String,
    pub files: Vec<AnnounceFileEntry>,
}

/// V3 package announce: the [`PackageAnnounceV2`] fields plus a durable batch
/// identity (`batch_uuid`) that survives re-attempts (a resend under a fresh
/// per-attempt `package_id`), letting the receiver keep exactly ONE row per
/// transfer instead of one per attempt.
///
/// Like [`PackageAnnounceV2`] this is a SEPARATE type carried by the appended
/// `Msg::Announce3` wire variant — the older announce structs' positional
/// postcard bytes stay frozen, so `Msg::Announce` (v1) and `Msg::Announce2` (v2)
/// still decode byte-for-byte for old peers. The app sender emits only v3; the
/// receive side accepts all three (v1/v2 fall back to the wire `package_id` as the
/// batch key). `package_id` is the CURRENT attempt's wire id (ack correlation);
/// `batch_uuid` is stable across the transfer's attempts. Byte image pinned by
/// `sharing::wire_golden_tests`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageAnnounceV3 {
    pub package_id: PackageId,
    pub root_hash: String,
    pub byte_size: u64,
    pub frame_count: u32,
    pub batch_name: String,
    pub batch_uuid: String,
    pub files: Vec<AnnounceFileEntry>,
}

/// Why a sender revoked an outstanding announce (spec §D2). A one-shot,
/// best-effort control signal (`Msg::Revoke`) telling the receiver to abort an
/// in-flight or pending transfer: `Cancelled` (the user cancelled the send),
/// `Superseded` (a newer attempt/batch replaces this one), or `Failed` (the
/// sender gave up). No consumer wires the abort yet (B4); this is the wire value.
///
/// A NEW enum — its variant order (`Cancelled` = 0, `Superseded` = 1,
/// `Failed` = 2) is a frozen positional contract on the wire, pinned by
/// `sharing::wire_golden_tests`. New variants may ONLY be appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RevokeReason {
    Cancelled,
    Superseded,
    Failed,
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
    ///
    /// `batch_name` / `files` are the v2 extras: `Some` when the announce arrived
    /// as `Msg::Announce2` / `Msg::Announce3` (or the loopback mock emulating a v2+
    /// send), `None` for a v1 `Msg::Announce` (an old peer, e.g. Perseus beta.3).
    ///
    /// `batch_uuid` is the durable per-transfer identity (spec §D2): a v3
    /// `Msg::Announce3` carries it on the wire; a v1/v2 announce predates it, so
    /// the receive path falls back to the wire `package_id` (guaranteeing ONE
    /// stable batch key on every path). The receiver logic that keys a row on it
    /// lands in a later task (B4); here it is threaded through.
    AnnounceReceived {
        from: NodeId,
        announce: PackageAnnounce,
        batch_name: Option<String>,
        batch_uuid: String,
        files: Option<Vec<AnnounceFileEntry>>,
    },
    /// A peer acknowledged a package we sent, returning per-frame receipts.
    AckReceived {
        from: NodeId,
        package_id: PackageId,
        receipts: Vec<FrameReceipt>,
    },
    /// A sender revoked an outstanding announce (spec §D2): abort the pending /
    /// in-flight transfer for `package_id` (the current attempt's wire id). The
    /// in-process counterpart of an inbound `Msg::Revoke`, delivered on the
    /// receive-role event stream just like [`AnnounceReceived`](Self::AnnounceReceived).
    /// No consumer wires the abort/cleanup yet (B4) — the receiver loop logs and
    /// ignores it for now (never dropped silently).
    RevokeReceived {
        from: NodeId,
        package_id: PackageId,
        reason: RevokeReason,
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
    /// Per-FILE upload progress for a package WE are serving (Task 2.2): the iroh
    /// provider-upload-events consumer attributes one payload GET's per-child
    /// transfer to a specific collection ENTRY **by hash-seq index** (offset ≥ 2 →
    /// entry `index-2`; never by hash — two byte-identical files share one blob
    /// hash but occupy distinct collection entries, so only the index
    /// disambiguates them), or the loopback mock emits one synthetic tick per
    /// file. Like [`ServeProgress`](Self::ServeProgress) /
    /// [`ServeComplete`](Self::ServeComplete) it originates LOCALLY — on OUR
    /// endpoint, never from a peer control message — so it carries no `from`.
    /// `file` is the entry's forward-slash `rel_path`; the sender engine reduces it
    /// to a basename and emits a send-side `sync-file-progress` keyed by the
    /// outbound row id. Best-effort UI data (progress, never a log or delivery
    /// truth).
    ServeFileProgress {
        package_id: PackageId,
        file: String,
        bytes_done: u64,
        bytes_total: u64,
    },
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
