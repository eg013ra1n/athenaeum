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
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use iroh::endpoint::{presets, Connection};
use iroh::protocol::{ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey};
use iroh_blobs::api::Store;
use iroh_blobs::protocol::ChunkRangesSeq;
use iroh_blobs::provider::events::{
    AbortReason, ConnectMode, EventMask, EventSender, ProviderMessage, RequestMode, RequestUpdate,
    ThrottleMode,
};
use iroh_blobs::store::fs::options::Options as FsOptions;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::store::mem::MemStore;
use iroh_blobs::store::GcConfig;
use iroh_blobs::{BlobsProtocol, Hash};
use iroh_tickets::endpoint::EndpointTicket;
use tokio::sync::mpsc;

use super::types::{
    AnnounceFileEntry, FrameReceipt, NodeId, PackageAnnounce, PackageAnnounceV3, PackageAnnounceV4,
    PackageId, PackageLayout, RevokeReason, StartInfo, TransportEvent,
};
use super::{FetchSink, ImportProgressSink, SharingTransport};
use crate::sync::DedupResponder;

pub(crate) mod assign;
pub mod blobs;
pub mod collab_pool;
pub mod node;
pub mod pacer;
pub mod proto;
pub(crate) mod telemetry;

#[cfg(test)]
mod tests;

use pacer::{UploadClass, UploadPacer};
use proto::{announce_received_from_msg, Msg, OfferEntry};

/// Custom ALPN for the announce/ack control channel. Distinct from
/// [`iroh_blobs::ALPN`] so the two protocols coexist on one endpoint.
///
/// **Load-bearing version signal (TECH-1).** The trailing `/1` is the wire
/// protocol version. The control wire is postcard with POSITIONAL encoding
/// (`proto::Msg` variants keyed by declaration index, struct fields by order —
/// no field names), so any BREAKING change — reordering/removing a `Msg` variant,
/// changing a field's type/order, renumbering `ReceiptOutcome` — makes old peers
/// silently decode new bytes into the wrong shape with no error, surfacing only
/// as an undiagnosable stalled transfer. The golden-byte guard
/// (`sharing::wire_golden_tests`) fails the build on exactly that drift; when it
/// does, BUMP THIS SUFFIX (`athenaeum/sync/1` → `.../2`) so mismatched peers fail
/// the ALPN handshake LOUDLY at connect time instead of decode-dying, and add a
/// new golden set. Additive-only changes (appending a new `Msg` variant at the
/// END, as `ProjectAnnounce`/`ProjectRequest` were) do not require a bump.
pub const SYNC_ALPN: &[u8] = b"athenaeum/sync/1";

/// ALPN of the collab blob store (collab v3 wave 2, P1): the second iroh-blobs
/// store under `<Collaboration root>/.athenaeum/blobs`, served by the same node
/// beside the personal store's [`iroh_blobs::ALPN`]. A distinct ALPN keeps the
/// two stores' hash spaces apart: a GET on one never reads the other. The
/// protocol spoken on it is stock iroh-blobs, unchanged.
pub const COLLAB_BLOBS_ALPN: &[u8] = b"athenaeum/collab-blobs/1";

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
pub(crate) const GC_INTERVAL: Duration = Duration::from_secs(900);

/// How often the COLLAB store's GC runs (Task 15, T9 ruling / P31): a
/// deleted replica's dead entry is dropped at most this long after its seed
/// tag went at settle, so a single delete re-fetches within settle + one
/// interval. Every in-flight collab fetch holds its in-flight tag (P22), so
/// a short interval never races a transfer. Measured cheap (see the Task 15
/// report: a GC run over 5,000 entries).
pub(crate) const COLLAB_GC_INTERVAL: Duration = Duration::from_secs(60);

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

/// Shared, late-bindable slot for the dedup [`DedupResponder`]. Same shape as
/// [`SharedConnectGate`]: cloned into [`SyncControlProtocol`] at construction so
/// an already-spawned router can have its responder installed afterwards. The
/// [`IrohTransport`] fills it once at build time (from its `responder` arg); the
/// shared node ([`node::SharedIrohNode`]) leaves it empty until the receiver
/// migrates onto it and calls
/// [`set_dedup_responder`](node::SharedIrohNode::set_dedup_responder). Absent ⇒
/// offers are answered want-all (nothing silently withheld).
pub(crate) type SharedResponder = Arc<Mutex<Option<Arc<dyn DedupResponder>>>>;

/// Host callback fired when a peer announces its presence (D1 §3.2).
///
/// Called with the **authenticated** sender id — the connection's `remote_id`,
/// never anything self-reported (the message has no payload). The host decides
/// what it means; the transport's only job is to hand it over. It runs on the
/// control accept loop, so an implementation must return promptly and do any real
/// work on its own task.
pub type PresenceHook = Arc<dyn Fn(NodeId) + Send + Sync>;

/// Shared, late-bindable slot for the [`PresenceHook`]. Same shape and lifetime
/// story as [`SharedResponder`]: cloned into [`SyncControlProtocol`] at
/// construction so an already-spawned router can have its hook installed
/// afterwards, via
/// [`set_presence_hook`](node::SharedIrohNode::set_presence_hook).
pub(crate) type SharedPresenceHook = Arc<Mutex<Option<PresenceHook>>>;

/// Resolves a served collection's root hash back to the [`PackageId`] whose
/// [`serve`](SharingTransport::serve) registered it — so the provider-upload-events
/// consumer (Task 13) can label an outgoing [`ServeProgress`](TransportEvent::ServeProgress).
/// `None` for a child-blob / hash-seq-internal / foreign hash (drained but not
/// emitted). Cloned into the consumer task at router build; each construction site
/// supplies its own (the legacy [`IrohTransport`] resolves nothing, the shared node
/// scans its role-prefixed `served` map).
pub(crate) type ServeRootResolver = Arc<dyn Fn(Hash) -> Option<PackageId> + Send + Sync>;

/// Resolves a served collection's `(root hash, hash-seq index)` to the collection
/// ENTRY at that index (Task 2.2) — `index-2 → entries[index-2]`, returning its
/// `(rel_path, byte_size)`; indices 0 (root hash-seq blob) and 1 (iroh
/// `CollectionMeta`) resolve `None`. Attribution is STRICTLY by index, never by
/// hash: two byte-identical files share one blob hash but occupy distinct
/// collection entries, so only the index disambiguates them. Cloned into the
/// consumer task at router build beside the [`ServeRootResolver`]; each
/// construction site supplies its own (the legacy [`IrohTransport`] resolves
/// nothing, the shared node scans its role-prefixed `served_files` map).
pub(crate) type ServeFileResolver = Arc<dyn Fn(Hash, u64) -> Option<(String, u64)> + Send + Sync>;

/// Depth of the provider-events channel feeding the upload-progress consumer
/// (Task 13). Per-connection/per-request notify messages are low volume; the
/// per-request transfer updates ride their own dedicated channels (drained by a
/// task each), so this need only absorb bursts of new-request notifications.
const PROVIDER_EVENT_CAPACITY: usize = 64;

/// Minimum wall-clock gap between two outgoing [`ServeProgress`](TransportEvent::ServeProgress)
/// ticks for one transfer (Task 13). Provider `Progress` events arrive ~per 16 KiB;
/// throttling to this cadence keeps the byte progress coarse (UI data, never a log
/// or a per-chunk event). A transfer shorter than one window still yields one tick
/// (a final flush at channel close).
const SERVE_PROGRESS_THROTTLE: Duration = Duration::from_millis(300);

/// Cumulative-byte accumulator for one served collection's upload progress
/// (Task 13 fix — a reviewer-caught regression in the first cut).
///
/// `RequestUpdate::Progress.end_offset` is PER-BLOB, not cumulative across a
/// collection: `handle_get_impl` (iroh-blobs 0.103 `provider.rs`) calls
/// `send_blob` once per hash-seq child (the hash-seq blob itself, then the
/// manifest, then every frame payload), and each call's `write_with_progress`
/// (`api/blobs.rs`) fires a fresh `Started` on `EncodedItem::Size` and reports
/// `notify_payload_write(index, leaf.offset, len)` where `leaf.offset` is the
/// BAO leaf offset WITHIN THAT BLOB — it restarts near 0 for every blob. Naively
/// tracking `sent.max(end_offset)` across the whole stream therefore captures
/// only the single LARGEST blob's size and freezes there for the rest of the
/// transfer (a multi-file frame set's progress bar would climb to its biggest
/// file and stop, e.g. 40 MB / 800 MB, forever).
///
/// This accumulator folds each blob's final offset into a running `base` the
/// moment the NEXT blob's `Started` arrives, so [`total`](Self::total) is a true
/// collection-wide cumulative figure.
#[derive(Default)]
pub(crate) struct UploadAccumulator {
    /// Sum of every blob's final offset, for every blob that has since been
    /// superseded by a later `Started`.
    base: u64,
    /// The current (most recently started) blob's highest-seen `end_offset`.
    cur: u64,
}

impl UploadAccumulator {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A new blob's transfer began: fold the previous blob's final offset into
    /// the running base and reset the per-blob counter. A no-op-safe first call
    /// (base and cur both start at 0).
    pub(crate) fn on_started(&mut self) {
        self.base += self.cur;
        self.cur = 0;
    }

    /// The current blob's progress ticked to `end_offset`. `max`-guarded (not
    /// assigned) so an out-of-order or duplicate tick within one blob can never
    /// regress the running total.
    pub(crate) fn on_progress(&mut self, end_offset: u64) {
        self.cur = self.cur.max(end_offset);
    }

    /// Cumulative bytes sent across every blob of this request so far.
    pub(crate) fn total(&self) -> u64 {
        self.base + self.cur
    }
}

#[cfg(test)]
mod upload_accumulator_tests {
    use super::UploadAccumulator;

    /// The regression the reviewer caught: naive `max(end_offset)` across a
    /// whole collection freezes the reported total at the single largest blob's
    /// size. `UploadAccumulator` folds each blob's final offset into a running
    /// base on `Started`, so a two-blob sequence's total reflects BOTH blobs —
    /// 372 + 372 = 744, not 372.
    #[test]
    fn accumulates_across_multiple_blobs() {
        let mut acc = UploadAccumulator::new();
        acc.on_started(); // blob 1 begins
        acc.on_progress(10);
        acc.on_progress(372); // blob 1 finishes at offset 372
        acc.on_started(); // blob 2 begins; blob 1's 372 folds into base
        acc.on_progress(5);
        acc.on_progress(372); // blob 2 finishes at offset 372 too
        assert_eq!(
            acc.total(),
            744,
            "cumulative total across two 372-byte blobs, not the single largest blob's size"
        );
    }

    /// Three blobs of distinct sizes sum correctly (guards against an
    /// off-by-one in the fold-on-Started ordering).
    #[test]
    fn accumulates_across_three_blobs_of_distinct_sizes() {
        let mut acc = UploadAccumulator::new();
        acc.on_started();
        acc.on_progress(100); // blob 1: 100 bytes
        acc.on_started();
        acc.on_progress(50);
        acc.on_progress(250); // blob 2: 250 bytes
        acc.on_started();
        acc.on_progress(400); // blob 3: 400 bytes
        assert_eq!(acc.total(), 750);
    }

    /// Out-of-order / duplicate progress within one blob never regresses the
    /// running total (defensive `max`).
    #[test]
    fn progress_within_one_blob_is_monotonic() {
        let mut acc = UploadAccumulator::new();
        acc.on_started();
        acc.on_progress(100);
        acc.on_progress(50); // a stale/out-of-order tick must not regress
        assert_eq!(acc.total(), 100);
    }

    #[test]
    fn empty_accumulator_totals_zero() {
        assert_eq!(UploadAccumulator::new().total(), 0);
    }
}

/// One per-file upload-progress tick: `(rel_path, bytes_done, bytes_total)`.
pub(crate) type FileTick = (String, u64, u64);

/// Per-FILE upload attribution for one served collection request (Task 2.2), the
/// sibling of [`UploadAccumulator`]. Tracks which collection ENTRY the current
/// hash-seq child maps to (resolved BY INDEX, never by hash) and its byte total,
/// so the provider-events consumer can drive a per-file [`ServeFileProgress`] bar.
///
/// Pure + time-free (the consumer owns throttling): each method returns the tick
/// to emit, if any. [`on_started`](Self::on_started) returns the PREVIOUS file's
/// terminal (100%) tick — so a file that transferred inside one throttle window
/// still completes its bar — then adopts the new child's entry (or `None` for a
/// hash-seq-internal child at index 0/1). [`on_progress`](Self::on_progress)
/// returns the current file's live tick. [`finish`](Self::finish) returns the
/// last file's terminal tick — the terminal `Completed` has no next `Started` to
/// flush it. `None` throughout for a non-payload child, so nothing spurious ever
/// emits.
#[derive(Default)]
pub(crate) struct ServeFileTracker {
    /// The current child's `(rel_path, byte_total)`, or `None` for a
    /// hash-seq-internal child (index 0/1) that maps to no collection entry.
    cur: Option<(String, u64)>,
}

impl ServeFileTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A new hash-seq child's transfer began. Returns the PREVIOUS file's terminal
    /// (100%) tick to flush before switching, then adopts `entry` — the resolver's
    /// `index-2 → (rel_path, size)` for a payload child, or `None` for index 0/1.
    pub(crate) fn on_started(&mut self, entry: Option<(String, u64)>) -> Option<FileTick> {
        let flush = self.cur.take().map(|(name, total)| (name, total, total));
        self.cur = entry;
        flush
    }

    /// The current child ticked to `end_offset` (per-blob — it restarts near 0 for
    /// each child, so it IS the current file's own offset). Returns the current
    /// file's live tick (`end_offset` clamped to its total), or `None` when the
    /// current child maps to no payload entry.
    pub(crate) fn on_progress(&self, end_offset: u64) -> Option<FileTick> {
        self.cur
            .as_ref()
            .map(|(name, total)| (name.clone(), end_offset.min(*total), *total))
    }

    /// The request completed. Returns the LAST file's terminal (100%) tick — it has
    /// no next `Started` to flush it — and clears the tracker.
    pub(crate) fn finish(&mut self) -> Option<FileTick> {
        self.cur.take().map(|(name, total)| (name, total, total))
    }
}

#[cfg(test)]
mod serve_file_tracker_tests {
    use super::ServeFileTracker;

    fn e(name: &str, size: u64) -> Option<(String, u64)> {
        Some((name.to_string(), size))
    }

    /// The core Task 2.2 shape: two payload children in hash-seq order. The FIRST
    /// child's terminal (100%) tick is flushed when the SECOND child's `Started`
    /// arrives (so a short file still completes its bar), and `finish` flushes the
    /// LAST child at 100% (the terminal `Completed` has no next `Started`).
    #[test]
    fn flushes_previous_file_on_next_started_and_last_on_finish() {
        let mut t = ServeFileTracker::new();
        // Child A begins (index 2 → a.fits, 100 bytes): no previous file to flush.
        assert_eq!(t.on_started(e("a.fits", 100)), None);
        // A live tick mid-transfer.
        assert_eq!(t.on_progress(40), Some(("a.fits".to_string(), 40, 100)));
        // Child B begins (index 3 → b.fits, 250): A is flushed at 100% first.
        assert_eq!(
            t.on_started(e("b.fits", 250)),
            Some(("a.fits".to_string(), 100, 100))
        );
        // Completed: B is flushed at 100%.
        assert_eq!(t.finish(), Some(("b.fits".to_string(), 250, 250)));
        // A second finish is a no-op (tracker cleared).
        assert_eq!(t.finish(), None);
    }

    /// A SHORT file that never reported a live `Progress` still completes its bar:
    /// the flush-on-next-`Started` emits its terminal tick even though `on_progress`
    /// was never called for it.
    #[test]
    fn short_file_completes_via_flush_without_any_progress() {
        let mut t = ServeFileTracker::new();
        assert_eq!(t.on_started(e("short.fits", 12)), None);
        // No on_progress for short.fits at all; the next Started flushes it at 100%.
        assert_eq!(
            t.on_started(e("next.fits", 30)),
            Some(("short.fits".to_string(), 12, 12))
        );
    }

    /// Hash-seq-internal children (index 0 = root, index 1 = `CollectionMeta`)
    /// resolve to `None` and never produce a tick — not on start, progress, or the
    /// flush when the first payload child begins.
    #[test]
    fn hash_seq_internal_children_never_tick() {
        let mut t = ServeFileTracker::new();
        assert_eq!(t.on_started(None), None); // index 0 (root hash-seq)
        assert_eq!(t.on_progress(999), None); // progress on a non-payload child
        assert_eq!(t.on_started(None), None); // index 1 (CollectionMeta), still None
        assert_eq!(t.on_progress(999), None);
        // First payload child (index 2): the previous (meta) child flushes nothing.
        assert_eq!(t.on_started(e("first.fits", 64)), None);
        assert_eq!(t.on_progress(64), Some(("first.fits".to_string(), 64, 64)));
    }

    /// Two byte-identical files (same size, would share one blob hash) are tracked
    /// as DISTINCT entries — attribution is by index, so each gets its own bar
    /// under its own `rel_path`. The tracker only ever sees the resolver's
    /// per-index entry, so a duplicate hash can never collapse them here.
    #[test]
    fn duplicate_sized_files_stay_distinct_by_entry() {
        let mut t = ServeFileTracker::new();
        assert_eq!(t.on_started(e("dup_a.fits", 4096)), None);
        assert_eq!(
            t.on_started(e("dup_c.fits", 4096)),
            Some(("dup_a.fits".to_string(), 4096, 4096))
        );
        assert_eq!(t.finish(), Some(("dup_c.fits".to_string(), 4096, 4096)));
    }

    /// An out-of-order / over-run `end_offset` is clamped to the file's total, so a
    /// per-file bar never reports past 100%.
    #[test]
    fn progress_is_clamped_to_total() {
        let mut t = ServeFileTracker::new();
        t.on_started(e("f.fits", 50));
        assert_eq!(t.on_progress(80), Some(("f.fits".to_string(), 50, 50)));
    }
}

/// Whether a GET request's ranges reach a collection **payload** — i.e. request
/// a non-empty range at any hash-seq offset ≥ 2 (Task 2.1). In an iroh-blobs
/// collection, offset 0 is the root hash-seq blob and offset 1 is the iroh
/// `CollectionMeta`; offsets ≥ 2 are the collection's file entries (frame
/// payloads + the manifest). So a phase-1 root+meta pull (offsets 0–1) and a
/// `fetch_manifest` probe's first GET are NOT payload-carrying, while the phase-2
/// hash-seq pull — and a resumed fetch that requests only the still-missing
/// frame offsets — ARE.
///
/// The transport labels serve-progress and serve-complete off this predicate:
/// only a payload-carrying request emits `ServeProgress` ticks (killing the
/// manifest-probe spurious tick) or routes a terminal `ServeComplete`.
///
/// [`ChunkRangesSeq::iter_non_empty_infinite`] yields only the non-empty
/// `(offset, ranges)` pairs and is INFINITE when the request repeats a non-empty
/// tail forever (e.g. `all` / a whole hash-seq). The first offset ≥ 2 is a
/// definitive yes, so the early return bounds the scan at offset 2 — a finite,
/// non-payload request instead exhausts its (short) non-empty prefix and yields
/// `false`.
fn request_is_payload_carrying(ranges: &ChunkRangesSeq) -> bool {
    for (offset, _) in ranges.iter_non_empty_infinite() {
        if offset >= 2 {
            return true;
        }
    }
    false
}

/// Evaluate the shared connect gate for `from`. Absent gate ⇒ admit (accept-all,
/// today's behavior). The gate `Arc` is cloned out from under the lock BEFORE the
/// predicate runs, so a gate that does blocking catalog I/O never holds the mutex
/// across that work.
pub(crate) fn connect_gate_admits(gate: &SharedConnectGate, from: &NodeId) -> bool {
    let predicate = gate.lock().expect("connect_gate mutex poisoned").clone();
    match predicate {
        Some(g) => g(from),
        None => true,
    }
}

/// Mount both ALPNs on `endpoint` and spawn the accept loop — the single
/// construction shape shared by the legacy [`IrohTransport::new`], the shared
/// node's [`bind`](node::SharedIrohNode) and its relay [`rebuild`]. Each of the
/// three sites previously hand-built the identical
/// [`GatedBlobs`] + [`SyncControlProtocol`] + [`Router`] triple; the only
/// per-site variation is threaded through the parameters:
///
/// - `sink` — [`EventSink::Direct`] for the legacy owned-endpoint transport,
///   [`EventSink::Demux`] for the shared node (both bind and rebuild).
/// - `flush_store_on_shutdown` — `true` only for [`IrohTransport`], whose router
///   teardown IS its store flush; `false` for the shared node, whose store is
///   shared across relay rebuilds and flushed explicitly at node shutdown (T8).
/// - `collab` — the shared node's [`CollabSlotBlobs`], mounted on
///   [`COLLAB_BLOBS_ALPN`] for the node's lifetime (collab v3 wave 2, P1/R4);
///   `None` for the legacy transport, which serves no collab store.
///
/// The `gate` and `responder` slots are cloned in (late-bindable), and the store
/// is borrowed (the caller keeps ownership for its own struct field). Behaviour
/// is byte-identical to the three inlined copies it replaces.
///
/// [`rebuild`]: node::SharedIrohNode
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_router(
    endpoint: Endpoint,
    store: &Store,
    gate: &SharedConnectGate,
    sink: EventSink,
    responder: SharedResponder,
    presence: SharedPresenceHook,
    flush_store_on_shutdown: bool,
    serve_resolver: ServeRootResolver,
    serve_file_resolver: ServeFileResolver,
    pacer: Arc<UploadPacer>,
    collab: Option<CollabSlotBlobs>,
) -> Router {
    // Provider upload events (Task 13): the masked channel is factored into
    // `provider_event_channel` so the collab store's provider (collab v3 wave 2,
    // P1) gets the SAME mask; its consumer is its own
    // (`spawn_collab_provider_events`, wave 3 — the serve check) but shares
    // this one `pacer`.
    let (events, rx) = provider_event_channel();
    // The personal store's accepted connections, so the consumer can name the
    // peer behind a request's `connection_id` (a refused push is logged with
    // its `from`).
    let conns = Arc::new(ConnRegistry::new("personal"));
    // Wrap the blobs provider so an ungated peer never receives a blob byte:
    // `GatedBlobs` checks the connect gate against the dialing node id before
    // delegating to the inner `iroh_blobs` handler (finding F5 hardening).
    let blobs = GatedBlobs {
        inner: BlobsProtocol::new(store, Some(events)),
        gate: Arc::clone(gate),
        flush_store_on_shutdown,
        conns: Arc::clone(&conns),
    };

    // The provider-events consumer (see `spawn_provider_events` for the
    // load-bearing drain + throttle-reply rules).
    spawn_provider_events(
        rx,
        pacer,
        sink.clone(),
        serve_resolver,
        serve_file_resolver,
        conns,
    );

    let control = SyncControlProtocol {
        sink,
        presence,
        responder,
        gate: Arc::clone(gate),
    };
    // Both protocols on one router; `spawn` registers every ALPN on the endpoint.
    // The collab-blobs ALPN (collab v3 wave 2, P1/R4) is mounted HERE, once, for
    // the node's lifetime: a router cannot add protocols after spawn, and
    // rebuilding one closes the endpoint (iroh `Router::shutdown`) or aborts every
    // live handler (dropping it). So the handler reads a slot instead, and a
    // Collaboration-root change swaps the slot, never the router.
    let builder = Router::builder(endpoint)
        .accept(iroh_blobs::ALPN, blobs)
        .accept(SYNC_ALPN, control);
    match collab {
        Some(handler) => builder.accept(COLLAB_BLOBS_ALPN, handler),
        None => builder,
    }
    .spawn()
}

/// The masked provider-event channel every blobs provider on this node uses
/// (the personal store's in [`build_router`], the collab store's in
/// [`node::SharedIrohNode::bind_with`]). One mask, one definition: the
/// collab store's consumer is [`spawn_collab_provider_events`], which puts
/// the serve check behind the same `get: InterceptLog`.
pub(crate) fn provider_event_channel() -> (EventSender, mpsc::Receiver<ProviderMessage>) {
    // Provider upload events (Task 13): a masked `EventSender` feeds a per-process
    // consumer that turns a peer's collection pull into outgoing byte progress.
    //
    // SECURITY (fix-personal-store-refuse-push, 2026-09-25): `get` is
    // `InterceptLog`, not the field's own type name — in iroh-blobs 0.103,
    // `EventSender::request()` (the path shared by get/get_many/push/observe)
    // matches on `self.mask.get` UNCONDITIONALLY, ignoring `mask.push` /
    // `mask.get_many` / `mask.observe` entirely (see `provider/events.rs`
    // `request()` — every request kind is dispatched through that one match).
    // So `mask.get` governs every request kind on this node, and it MUST be an
    // `Intercept*` mode or a push is silently admitted regardless of
    // `mask.push`'s value (this bit us: the mask used to read
    // `push: RequestMode::Disabled` via `..EventMask::DEFAULT`, and a push
    // still landed). `InterceptLog` (not plain `Intercept`) keeps the
    // per-request transfer events GET's upload-progress plumbing depends on.
    // The consumer (below) now replies `Err(AbortReason::Permission)` to
    // every `PushRequestReceived` and `Ok(())` to every other request kind —
    // that reply, not the mask, is what actually refuses a push.
    //
    // `throttle: ThrottleMode::Intercept` is ALWAYS on (W1) — it is never toggled
    // to match whether a limit is currently configured. The provider's writer
    // awaits an rpc reply from our consumer before every ~16 KiB payload write, so
    // DELAYING that reply IS the throttle; the mask itself is fixed at router
    // build, so gating it on the setting would mean rebinding the endpoint to
    // change a limit. Keeping it on makes the limit LIVE-updatable
    // (`UploadPacer::set_rate`) with no reconnect.
    //
    // The disabled path is ~free: with rate 0 `reserve` returns `Duration::ZERO`
    // after a single relaxed atomic load, and the consumer replies inline (no
    // spawn, no sleep) — one in-process rpc round trip per 16 KiB against a link
    // that is already doing QUIC framing + BLAKE3 verification on those same
    // bytes.
    //
    // ONE pacer per endpoint, deliberately: the budget is the whole DEVICE's sync
    // upload, shared across every peer and every concurrent GET, which is exactly
    // what the setting promises (and what keeps an observatory's uplink usable
    // while N peers pull at once).
    EventSender::channel(
        PROVIDER_EVENT_CAPACITY,
        EventMask {
            connected: ConnectMode::Notify,
            get: RequestMode::InterceptLog,
            throttle: ThrottleMode::Intercept,
            ..EventMask::DEFAULT
        },
    )
}

/// A detached pure-drain task for a request's update stream (LOAD-BEARING
/// SAFETY RULE of [`spawn_provider_events`]): keep reading until the sender
/// drops, never abort, never drop the receiver early. A macro, not a
/// function: the receiver's type lives in a crate we do not depend on.
macro_rules! drain_detached {
    ($rx:expr) => {{
        let mut updates = $rx;
        tokio::spawn(async move { while let Ok(Some(_)) = updates.recv().await {} });
    }};
}

/// Spawn the consumer that drains the PERSONAL store's provider event
/// stream: per-GET upload progress (resolved through `serve_resolver` /
/// `serve_file_resolver` and routed into `consumer_sink`) and the upload
/// throttle, paced by `pacer` as [`UploadClass::Personal`]. Every
/// payload-carrying GET holds a
/// [`PersonalUploadGuard`](pacer::PersonalUploadGuard) for its whole
/// transfer, so collab uploads yield to it (spec §8). The collab store has
/// its own consumer, [`spawn_collab_provider_events`], sharing the SAME
/// `pacer` (one device-wide cap).
///
/// The provider-events consumer lives beside the router (never woven into the
/// node internals): one task drains the masked event stream and, per GET
/// request, spawns a detached drain task.
///
/// LOAD-BEARING SAFETY RULE: every request-update receiver (`m.rx`) MUST be
/// drained for the whole life of its transfer. `iroh-blobs` sends transfer
/// progress with `try_send(..).await?` into that channel; a DROPPED receiver
/// makes the next send error and that `?` aborts the PEER'S DOWNLOAD. So every
/// rx-carrying message gets a detached drain task unconditionally — even an
/// unmapped/foreign hash, even a Notify-suffixed variant this mask can no
/// longer produce (kept as dead-code-safe defensive arms) — and we never
/// block, never drop early.
///
/// LOAD-BEARING SAFETY RULE #2: every request-reply oneshot (`m.tx`, on
/// `GetRequestReceived` / `GetManyRequestReceived` / `PushRequestReceived` /
/// `ObserveRequestReceived`) MUST get exactly one reply. A DROPPED `tx` aborts
/// the transfer the same way a dropped `Throttle` `tx` does. `Ok(())` admits
/// the request; `Err(AbortReason::Permission)` refuses it before any byte is
/// read or written for it (`provider.rs` checks this reply BEFORE calling into
/// the store) — that refusal is the ONLY place push requests are actually
/// rejected; see `provider_event_channel`'s doc for why the mask alone can't
/// do it in this library version.
///
/// The `Throttle` reply oneshot (`m.tx`) is load-bearing THE SAME WAY, and is
/// the reason this rule now has teeth on the hot path: with
/// `ThrottleMode::Intercept` the provider's writer awaits our reply before
/// every ~16 KiB payload write. ALWAYS reply, and always reply `Ok(())` — a
/// DROPPED `tx` errors the writer and an `Err(AbortReason::…)` reply
/// deliberately kills the transfer, so neither is ever a valid way to say
/// "slow down". The DELAY before the reply IS the throttle. And that delay is
/// slept on a SPAWNED task, never here: this consumer task also carries the
/// GET-request messages whose drain tasks keep every other in-flight transfer
/// alive, so sleeping inline would stall the whole node's uploads for one
/// paced chunk.
pub(crate) fn spawn_provider_events(
    mut rx: mpsc::Receiver<ProviderMessage>,
    pacer: Arc<UploadPacer>,
    consumer_sink: EventSink,
    serve_resolver: ServeRootResolver,
    serve_file_resolver: ServeFileResolver,
    conns: Arc<ConnRegistry>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // A detached pure-drain task for any rx-carrying message we don't emit for
        // (mask-quirk defense): keep reading until the sender drops, never abort.
        macro_rules! drain_only {
            ($m:expr) => {{
                drain_detached!($m.rx);
            }};
        }
        while let Some(msg) = rx.recv().await {
            match msg {
                ProviderMessage::GetRequestReceived(m) => {
                    // Read-only request: admit it immediately. With
                    // `get: InterceptLog` the provider's own `get_request()`
                    // await is blocked on this reply (SAFETY RULE #2) — replying
                    // right away, before any progress bookkeeping, keeps a GET's
                    // start latency the same as it was under the old
                    // notify-only mode.
                    m.tx.send(Ok(())).await.ok();
                    // Collection root + requested ranges of this GET (Task 2.1).
                    // The resolver maps the root to the package we announced, or
                    // `None` (child blob / hash-seq internal / foreign hash — e.g.
                    // a manifest probe's second GET targets the manifest's own raw
                    // hash). A request is PAYLOAD-CARRYING iff its ranges reach a
                    // collection file entry (offset >= 2); a phase-1 root+meta pull
                    // and a manifest probe's first GET are not. Only a
                    // payload-carrying request that resolves to a served package
                    // emits progress or a terminal complete — every other request
                    // is still drained fully (SAFETY RULE), just silently.
                    let root: Hash = m.inner.request.hash;
                    let payload_carrying = request_is_payload_carrying(&m.inner.request.ranges);
                    let mut updates = m.rx;
                    let resolver = Arc::clone(&serve_resolver);
                    let file_resolver = Arc::clone(&serve_file_resolver);
                    let sink = consumer_sink.clone();
                    // A personal upload is running for as long as this
                    // payload-carrying transfer drains (spec §8): collab
                    // uploads yield to it. Taken before the spawn so a
                    // throttle of this transfer's first chunk already sees it.
                    let active = payload_carrying.then(|| pacer.personal_upload());
                    tokio::spawn(async move {
                        let _active = active;
                        // The single emit gate: `Some(pkg)` only when the request
                        // both carries payload AND resolves to one of our packages.
                        // ALL per-file emits are additionally gated on this, so a
                        // non-payload request (or one for a foreign hash) never
                        // routes a `ServeFileProgress` — not even on the flush.
                        let emit = if payload_carrying {
                            resolver(root)
                        } else {
                            None
                        };
                        // Cumulative across every blob in this request (Task 13
                        // fix) — see `UploadAccumulator` docs for why a naive
                        // `max(end_offset)` over the raw stream is wrong.
                        let mut acc = UploadAccumulator::new();
                        let mut last = Instant::now();
                        // Per-FILE attribution (Task 2.2): the pure tracker maps the
                        // current hash-seq child to its collection entry BY INDEX and
                        // yields the tick to emit; `file_last` throttles the live
                        // per-file ticks (reusing the same window as the batch tick).
                        let mut files = ServeFileTracker::new();
                        let mut file_last = Instant::now();
                        // Set once we flushed + signalled completion on a terminal
                        // `Completed`, so the close-time flush below never emits a
                        // stray `ServeProgress` AFTER the `ServeComplete` (which
                        // would flip the UI stage back to `transferring`).
                        let mut completed = false;
                        // Drain for the whole transfer; the loop ends when the
                        // sender drops (`Ok(None)`) or errors — the receiver is
                        // never dropped early (SAFETY RULE).
                        while let Ok(Some(update)) = updates.recv().await {
                            match update {
                                // A new blob's transfer began: fold the PREVIOUS
                                // blob's final offset into the running base before
                                // this blob's offsets start counting from ~0.
                                RequestUpdate::Started(t) => {
                                    acc.on_started();
                                    // Resolve THIS child's collection entry by its
                                    // hash-seq index (`index-2 → entry`; 0/1 → None),
                                    // pairing the name with the provider-reported
                                    // blob size (`t.size`). Only for a payload
                                    // request that resolves to one of our packages —
                                    // otherwise no per-file entry is ever adopted, so
                                    // the flush below can't emit for a non-payload
                                    // request. The tracker returns the PREVIOUS
                                    // file's terminal (100%) tick to flush.
                                    let entry = if emit.is_some() {
                                        file_resolver(root, t.index)
                                            .map(|(name, _bytes)| (name, t.size))
                                    } else {
                                        None
                                    };
                                    if let (Some(pkg), Some((file, done, total))) =
                                        (&emit, files.on_started(entry))
                                    {
                                        // A terminal per-file tick — emitted
                                        // unconditionally (bypasses the throttle) so a
                                        // file that finished inside one window still
                                        // completes its bar.
                                        file_last = Instant::now();
                                        sink.route_serve_file_progress(pkg, file, done, total);
                                    }
                                }
                                RequestUpdate::Progress(p) => {
                                    acc.on_progress(p.end_offset);
                                    if let Some(pkg) = &emit {
                                        if last.elapsed() >= SERVE_PROGRESS_THROTTLE {
                                            last = Instant::now();
                                            sink.route_serve_progress(pkg, acc.total());
                                        }
                                        // Throttled live per-file tick for the current
                                        // child (`p.end_offset` is per-blob, so it is
                                        // this file's own offset).
                                        if let Some((file, done, total)) =
                                            files.on_progress(p.end_offset)
                                        {
                                            if file_last.elapsed() >= SERVE_PROGRESS_THROTTLE {
                                                file_last = Instant::now();
                                                sink.route_serve_file_progress(
                                                    pkg, file, done, total,
                                                );
                                            }
                                        }
                                    }
                                }
                                // Terminal success of a payload-carrying serve: the
                                // peer pulled everything it asked for. Flush the LAST
                                // file at 100% (no next `Started` to trigger it), then
                                // the final cumulative figure, then signal
                                // upload-complete — the "uploaded — awaiting
                                // confirmation" stage. No byte-threshold guard: a
                                // resume legitimately serves fewer bytes than
                                // `byte_size`, so `Completed` alone is the signal.
                                RequestUpdate::Completed(_) => {
                                    if let Some(pkg) = &emit {
                                        if let Some((file, done, total)) = files.finish() {
                                            sink.route_serve_file_progress(pkg, file, done, total);
                                        }
                                        if acc.total() > 0 {
                                            sink.route_serve_progress(pkg, acc.total());
                                        }
                                        sink.route_serve_complete(pkg);
                                    }
                                    completed = true;
                                }
                                // Failure routes nothing (a distinct event, never a
                                // `ServeComplete`); the ack stays delivery truth. The
                                // current file is deliberately NOT flushed to 100% —
                                // an aborted child did not finish.
                                RequestUpdate::Aborted(_) => {}
                            }
                        }
                        // Close-time flush for a stream that ended WITHOUT a
                        // terminal `Completed` (a transfer shorter than one
                        // throttle window, or an abort): surface the final batch
                        // tick. Skipped after `Completed` (already flushed +
                        // completed). No per-file 100% flush here — an incomplete
                        // stream's last child did not finish.
                        if !completed {
                            if let Some(pkg) = &emit {
                                if acc.total() > 0 {
                                    sink.route_serve_progress(pkg, acc.total());
                                }
                            }
                        }
                    });
                }
                // Read-only, no progress routing needed: admit immediately
                // (SAFETY RULE #2), then drain-only (SAFETY RULE #1) — no emit.
                ProviderMessage::GetManyRequestReceived(m) => {
                    m.tx.send(Ok(())).await.ok();
                    drain_only!(m);
                }
                ProviderMessage::ObserveRequestReceived(m) => {
                    m.tx.send(Ok(())).await.ok();
                    drain_only!(m);
                }
                // SECURITY (fix-personal-store-refuse-push): the ONLY place a
                // push is actually refused — see `provider_event_channel`'s doc
                // for why the mask itself can't do it in iroh-blobs 0.103. The
                // reply happens BEFORE `handle_push_impl` ever runs on the
                // provider side (checked at `push_request()`, ahead of any
                // store write), so a refused push writes nothing.
                ProviderMessage::PushRequestReceived(m) => {
                    tracing::warn!(
                        from = %conns.peer_label(m.inner.connection_id),
                        store = "personal",
                        connection_id = m.inner.connection_id,
                        request_id = m.inner.request_id,
                        hash = %m.inner.request.hash,
                        "inbound push refused"
                    );
                    m.tx.send(Err(AbortReason::Permission)).await.ok();
                    drain_only!(m);
                }
                // Dead-code-safe defensive arms: `get: InterceptLog` never
                // produces a Notify-suffixed request variant (those are only
                // sent under `Notify`/`NotifyLog`), but they remain valid
                // `ProviderMessage` variants type-wise, so drain-only if a
                // future library version ever surprises us here (SAFETY RULE).
                ProviderMessage::GetRequestReceivedNotify(m) => drain_only!(m),
                ProviderMessage::GetManyRequestReceivedNotify(m) => drain_only!(m),
                ProviderMessage::PushRequestReceivedNotify(m) => drain_only!(m),
                ProviderMessage::ObserveRequestReceivedNotify(m) => drain_only!(m),
                // The upload throttle (W1). The provider is asking permission to
                // write `size` bytes (~16 KiB) and is BLOCKED on this reply; the
                // pacer says how long that permission should be withheld.
                //
                // Both branches end in `send(Ok(()))` — see the SAFETY RULE above:
                // dropping `tx` or replying `Err` aborts the peer's download, so
                // waiting is the only rate-limiting move available to us.
                ProviderMessage::Throttle(m) => {
                    let wait = pacer.reserve_class(m.inner.size, UploadClass::Personal);
                    if wait.is_zero() {
                        // Unlimited (rate 0) or simply within budget: reply inline.
                        // A local irpc oneshot never yields, so this costs no
                        // scheduling at all — that is what keeps the always-on mask
                        // free when the user has set no limit.
                        m.tx.send(Ok(())).await.ok();
                    } else {
                        // Paced: sleep on a DETACHED task. Never on this consumer —
                        // it also carries the messages that spawn every transfer's
                        // drain task (SAFETY RULE), so a sleep here would stall
                        // unrelated uploads node-wide.
                        let tx = m.tx;
                        tokio::spawn(async move {
                            tokio::time::sleep(wait).await;
                            tx.send(Ok(())).await.ok();
                        });
                    }
                }
                // No update channel to drain. A closed connection leaves the
                // registry (its peer no longer needs naming).
                ProviderMessage::ConnectionClosed(m) => conns.forget(m.inner.connection_id),
                ProviderMessage::ClientConnected(_) | ProviderMessage::ClientConnectedNotify(_) => {
                }
            }
        }
    })
}

/// The collab store's upload stream gauge (L11): how many collab gets are
/// being served right now, against the device's limit
/// (`collab.max_upload_streams`). A [`StreamPermit`] is held for the whole
/// transfer; the limit is live-updatable.
pub struct StreamGauge {
    in_use: std::sync::atomic::AtomicUsize,
    limit: std::sync::atomic::AtomicUsize,
}

impl StreamGauge {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            in_use: std::sync::atomic::AtomicUsize::new(0),
            limit: std::sync::atomic::AtomicUsize::new(limit),
        })
    }

    /// Take one stream if `in_use < limit` — atomically, so two requests
    /// admitted at once never exceed the limit (the loser is refused).
    pub fn try_acquire(self: &Arc<Self>) -> Option<StreamPermit> {
        use std::sync::atomic::Ordering;
        let limit = self.limit.load(Ordering::Relaxed);
        self.in_use
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < limit).then_some(n + 1)
            })
            .ok()
            .map(|_| StreamPermit {
                gauge: Arc::clone(self),
            })
    }

    pub fn in_use(&self) -> usize {
        self.in_use.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn limit(&self) -> usize {
        self.limit.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Apply a new limit to the next request; streams already running keep
    /// going (a lower limit only refuses new ones).
    pub fn set_limit(&self, n: usize) {
        self.limit.store(n, std::sync::atomic::Ordering::Relaxed);
    }
}

/// One served collab stream ([`StreamGauge::try_acquire`]); released on drop.
pub struct StreamPermit {
    gauge: Arc<StreamGauge>,
}

impl Drop for StreamPermit {
    fn drop(&mut self) {
        self.gauge
            .in_use
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// The node's late-bindable serve oracle slot (spec §9.3). `None` ⇒ no
/// record/stamp check: a bare node with no catalog behind it (the transport's
/// own tests) serves whatever its collab store holds, like the connect gate
/// admits everyone when none is installed. The upload stream limit (L11)
/// applies either way — past it a get is refused with `ERR_LIMIT`, which the
/// fetcher reads as "busy, retry later" (Task 12). The app installs the
/// catalog-backed oracle as soon as it binds the node
/// (`api::sync::ensure_iroh_node`).
pub type SharedServeOracle = Arc<RwLock<Option<Arc<dyn crate::collab::serve::ServeOracle>>>>;

/// The accepted connections of one blobs provider, keyed by the provider's
/// `connection_id` (`Connection::stable_id`, the id iroh-blobs puts on every
/// provider event): the peer behind each, and a weak handle to close it.
/// Registered by [`GatedBlobs::accept`] after the connect gate admitted the
/// peer; forgotten when the provider reports the connection closed (and
/// pruned of dead handles on every registration).
pub(crate) struct ConnRegistry {
    /// Which store's provider this registry belongs to (`"personal"` /
    /// `"collab"`), for its log lines.
    store: &'static str,
    conns: Mutex<HashMap<u64, (NodeId, iroh::endpoint::WeakConnectionHandle)>>,
}

impl ConnRegistry {
    pub(crate) fn new(store: &'static str) -> Self {
        Self {
            store,
            conns: Mutex::new(HashMap::new()),
        }
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<u64, (NodeId, iroh::endpoint::WeakConnectionHandle)>>
    {
        self.conns.lock().unwrap_or_else(|poisoned| {
            tracing::error!("connection registry poisoned; recovering it");
            poisoned.into_inner()
        })
    }

    pub(crate) fn register(&self, connection: &Connection) {
        let from: NodeId = *connection.remote_id().as_bytes();
        let mut conns = self.lock();
        conns.retain(|_, (_, h)| h.upgrade().is_some_and(|c| c.close_reason().is_none()));
        conns.insert(
            connection.stable_id() as u64,
            (from, connection.weak_handle()),
        );
    }

    /// Register a connection the connect gate just admitted, then ask the
    /// gate AGAIN: a membership change landing between the accept-time check
    /// and the registration would otherwise miss
    /// [`close_not_admitted`](Self::close_not_admitted) (its snapshot did not
    /// hold this connection yet). Returns `false` — the connection closed —
    /// when the peer is no longer admitted.
    pub(crate) fn register_admitted(
        &self,
        connection: &Connection,
        gate: &SharedConnectGate,
    ) -> bool {
        self.register(connection);
        let from: NodeId = *connection.remote_id().as_bytes();
        if connect_gate_admits(gate, &from) {
            return true;
        }
        self.forget(connection.stable_id() as u64);
        connection.close(0u32.into(), b"membership revoked");
        tracing::info!(
            peer = %hex32(&from),
            store = self.store,
            count = 1,
            "connections closed: no longer admitted"
        );
        false
    }

    pub(crate) fn forget(&self, connection_id: u64) {
        self.lock().remove(&connection_id);
    }

    /// The peer behind `connection_id`, if the connection is registered.
    pub(crate) fn peer_of(&self, connection_id: u64) -> Option<NodeId> {
        self.lock().get(&connection_id).map(|(node, _)| *node)
    }

    /// [`peer_of`](Self::peer_of) rendered for a `from` log field.
    pub(crate) fn peer_label(&self, connection_id: u64) -> String {
        self.peer_of(connection_id)
            .map(|n| hex32(&n))
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Close every registered connection whose peer `gate` no longer admits;
    /// returns how many were closed. The gate runs outside the registry lock
    /// (it may read the catalog).
    pub(crate) fn close_not_admitted(&self, gate: &SharedConnectGate) -> usize {
        let peers: HashSet<NodeId> = self.lock().values().map(|(node, _)| *node).collect();
        let dropped: HashSet<NodeId> = peers
            .into_iter()
            .filter(|node| !connect_gate_admits(gate, node))
            .collect();
        if dropped.is_empty() {
            return 0;
        }
        let mut closed_per_peer: HashMap<NodeId, usize> = HashMap::new();
        {
            let mut conns = self.lock();
            conns.retain(|_, (node, handle)| {
                if !dropped.contains(node) {
                    return true;
                }
                if let Some(c) = handle.upgrade() {
                    if c.close_reason().is_none() {
                        c.close(0u32.into(), b"membership revoked");
                        *closed_per_peer.entry(*node).or_default() += 1;
                    }
                }
                false
            });
        }
        let mut closed = 0;
        for (node, count) in closed_per_peer {
            closed += count;
            tracing::info!(
                peer = %hex32(&node),
                store = self.store,
                count,
                "connections closed: no longer admitted"
            );
        }
        closed
    }
}

/// Spawn the consumer of the COLLAB store's provider events (spec §8, §9.3,
/// plan P14/P15). The channel is [`provider_event_channel`] (its
/// `get: InterceptLog` routes every request kind here for a reply), and the
/// SAFETY RULES of [`spawn_provider_events`] hold unchanged: every request
/// reply oneshot gets exactly one reply, every update stream is drained for
/// the whole transfer, every throttle is answered `Ok(())` after its delay.
///
/// - **get** — refused unless it is a plain blob request (a request past
///   offset 0 would stream hash-seq children the check never sees), then the
///   serve check ([`collab_serve_verdict`]): the catalog row,
///   the file's stamp, storage availability and the upload stream limit
///   decide; a refusal is `ERR_PERMISSION`, or `ERR_LIMIT` past the limit.
///   Without an oracle only the stream limit applies. An admitted get holds
///   a [`StreamPermit`] until its transfer drains.
/// - **observe** — the same check without counting a stream.
/// - **push** — refused (`ERR_PERMISSION`), logged with the peer.
/// - **get-many** — refused: the collab exchange never sends one.
/// - **throttle** — paced by `pacer` as [`UploadClass::Collab`], so collab
///   uploads yield to an active personal upload.
pub(crate) fn spawn_collab_provider_events(
    mut rx: mpsc::Receiver<ProviderMessage>,
    pacer: Arc<UploadPacer>,
    oracle: SharedServeOracle,
    gauge: Arc<StreamGauge>,
    conns: Arc<ConnRegistry>,
    meter: Arc<crate::collab::live::meter::ExchangeMeter>,
) -> tokio::task::JoinHandle<()> {
    use crate::collab::live::meter::{device_id_of, FlowDirection};

    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                ProviderMessage::GetRequestReceived(m) => {
                    let oracle = current_oracle(&oracle);
                    let gauge = Arc::clone(&gauge);
                    let from = conns.peer_label(m.inner.connection_id);
                    // Captured BEFORE the spawn: the connection may already be
                    // forgotten (peer disconnected) by the time the task below
                    // runs its first await.
                    let peer = conns
                        .peer_of(m.inner.connection_id)
                        .map(|n| device_id_of(&n));
                    let key = format!("serve:{}:{}", m.inner.connection_id, m.inner.request_id);
                    let meter = Arc::clone(&meter);
                    tokio::spawn(async move {
                        let hash = m.inner.request.hash;
                        // Only a plain blob request is checked (and fetched by
                        // the collab exchange): a request reaching past offset
                        // 0 would stream a hash-seq's CHILDREN, which the serve
                        // check never sees — refused outright.
                        let verdict = if m.inner.request.ranges.is_blob() {
                            collab_serve_verdict(oracle, hash, &gauge, true, &from).await
                        } else {
                            tracing::warn!(
                                from = %from,
                                blake3 = %hash,
                                "collab get refused: not a plain blob request"
                            );
                            Err(AbortReason::Permission)
                        };
                        let (permit, record) = match verdict {
                            Ok((permit, rec)) => {
                                m.tx.send(Ok(())).await.ok();
                                (permit, rec)
                            }
                            Err(reason) => {
                                m.tx.send(Err(reason)).await.ok();
                                (None, None)
                            }
                        };
                        // Task 11: meter this serve only when it is an
                        // admitted, attributed get — a real peer behind the
                        // connection AND a catalog record for what it pulled.
                        let metered = match (&record, &peer) {
                            (Some(r), Some(_)) => {
                                let name = r
                                    .path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                meter.register(
                                    &key,
                                    FlowDirection::Send,
                                    &r.project_id,
                                    &r.frame_uuid,
                                    &name,
                                    0,
                                );
                                true
                            }
                            _ => false,
                        };
                        // SAFETY RULE: drain the update stream for the whole
                        // transfer; the permit is held until it ends — every
                        // branch keeps draining even when the get was not
                        // metered.
                        let mut updates = m.rx;
                        let mut acc = UploadAccumulator::new();
                        let mut reported = 0u64;
                        let mut completed = false;
                        while let Ok(Some(update)) = updates.recv().await {
                            if !metered {
                                continue;
                            }
                            match update {
                                RequestUpdate::Started(t) => {
                                    acc.on_started();
                                    meter.set_size(&key, t.size);
                                }
                                RequestUpdate::Progress(p) => {
                                    acc.on_progress(p.end_offset);
                                    let total = acc.total();
                                    if let Some(dev) = &peer {
                                        meter.delivered(
                                            &key,
                                            dev,
                                            total.saturating_sub(reported),
                                            std::time::Instant::now(),
                                        );
                                    }
                                    reported = reported.max(total);
                                }
                                RequestUpdate::Completed(_) => completed = true,
                                RequestUpdate::Aborted(_) => {}
                            }
                        }
                        if metered {
                            meter.finish(&key, completed, std::time::Instant::now());
                        }
                        drop(permit);
                    });
                }
                ProviderMessage::ObserveRequestReceived(m) => {
                    let oracle = current_oracle(&oracle);
                    let gauge = Arc::clone(&gauge);
                    let from = conns.peer_label(m.inner.connection_id);
                    tokio::spawn(async move {
                        let hash = m.inner.request.hash;
                        let verdict =
                            collab_serve_verdict(oracle, hash, &gauge, false, &from).await;
                        m.tx.send(verdict.map(|_| ())).await.ok();
                        let mut updates = m.rx;
                        while let Ok(Some(_)) = updates.recv().await {}
                    });
                }
                ProviderMessage::PushRequestReceived(m) => {
                    tracing::warn!(
                        from = %conns.peer_label(m.inner.connection_id),
                        store = "collab",
                        connection_id = m.inner.connection_id,
                        request_id = m.inner.request_id,
                        hash = %m.inner.request.hash,
                        "inbound push refused"
                    );
                    m.tx.send(Err(AbortReason::Permission)).await.ok();
                    drain_detached!(m.rx);
                }
                ProviderMessage::GetManyRequestReceived(m) => {
                    tracing::debug!(
                        from = %conns.peer_label(m.inner.connection_id),
                        connection_id = m.inner.connection_id,
                        "collab get-many refused"
                    );
                    m.tx.send(Err(AbortReason::Permission)).await.ok();
                    drain_detached!(m.rx);
                }
                ProviderMessage::Throttle(m) => {
                    let wait = pacer.reserve_class(m.inner.size, UploadClass::Collab);
                    if wait.is_zero() {
                        m.tx.send(Ok(())).await.ok();
                    } else {
                        // Never sleep on this consumer (see
                        // `spawn_provider_events`'s SAFETY RULE).
                        let tx = m.tx;
                        tokio::spawn(async move {
                            tokio::time::sleep(wait).await;
                            tx.send(Ok(())).await.ok();
                        });
                    }
                }
                // Dead-code-safe defensive arms, as in `spawn_provider_events`.
                ProviderMessage::GetRequestReceivedNotify(m) => drain_detached!(m.rx),
                ProviderMessage::GetManyRequestReceivedNotify(m) => drain_detached!(m.rx),
                ProviderMessage::PushRequestReceivedNotify(m) => drain_detached!(m.rx),
                ProviderMessage::ObserveRequestReceivedNotify(m) => drain_detached!(m.rx),
                // `connected: Notify` never produces the intercepted variant;
                // answer it anyway so a future mask change cannot wedge a
                // connection on an unanswered oneshot.
                ProviderMessage::ClientConnected(m) => {
                    m.tx.send(Ok(())).await.ok();
                }
                ProviderMessage::ConnectionClosed(m) => conns.forget(m.inner.connection_id),
                ProviderMessage::ClientConnectedNotify(_) => {}
            }
        }
    })
}

/// The installed oracle, cloned out from under the lock (never held across
/// an await). A poisoned slot is logged and read through.
fn current_oracle(slot: &SharedServeOracle) -> Option<Arc<dyn crate::collab::serve::ServeOracle>> {
    match slot.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => {
            tracing::error!("collab serve oracle slot poisoned; reading through it");
            poisoned.into_inner().clone()
        }
    }
}

/// The serve check of one collab get/observe (spec §9.3). The oracle's
/// catalog read and the file stat run on a blocking thread. Without an
/// oracle only the upload stream limit is checked (see
/// [`SharedServeOracle`]). `Ok((Some(permit), rec))` admits a counted get;
/// `Ok((None, rec))` an observe or a get with `count_stream == false`. `rec`
/// is the looked-up [`ServeRecord`](crate::collab::serve::ServeRecord)
/// whenever an oracle is installed and found one — `None` when there is no
/// oracle to ask (Task 11: the caller meters an admitted get by it).
async fn collab_serve_verdict(
    oracle: Option<Arc<dyn crate::collab::serve::ServeOracle>>,
    hash: Hash,
    gauge: &Arc<StreamGauge>,
    count_stream: bool,
    from: &str,
) -> Result<
    (
        Option<StreamPermit>,
        Option<crate::collab::serve::ServeRecord>,
    ),
    AbortReason,
> {
    use crate::collab::serve::{decide, ServeDecision};
    use crate::collab::storage::sweep::Stamp;

    let blake3_hex = hash.to_hex().to_string();
    let limit = if count_stream {
        gauge.limit()
    } else {
        usize::MAX
    };
    let (decision, rec, oracle) = match oracle {
        // No record/stamp check (see `SharedServeOracle`), but a counted get
        // still takes a stream under the limit — or is refused with
        // `ERR_LIMIT`, which the fetcher retries (Task 12, ruling R2).
        None if !count_stream => return Ok((None, None)),
        None => {
            return match gauge.try_acquire() {
                Some(permit) => Ok((Some(permit), None)),
                None => {
                    tracing::debug!(
                        from,
                        streams = gauge.in_use(),
                        "collab get refused: upload stream limit"
                    );
                    Err(AbortReason::RateLimited)
                }
            };
        }
        Some(oracle) => {
            let o = Arc::clone(&oracle);
            let hex = blake3_hex.clone();
            let checked = tokio::task::spawn_blocking(move || {
                let rec = o.lookup(&hex);
                let observed = rec.as_ref().and_then(|r| match std::fs::metadata(&r.path) {
                    Ok(meta) => Some(Stamp::of(&meta)),
                    Err(e) => {
                        tracing::debug!(path = %r.path.display(), error = %e, "serve check: stat failed");
                        None
                    }
                });
                (rec, observed, o.serving())
            })
            .await;
            let (rec, observed, serving) = match checked {
                Ok(checked) => checked,
                Err(e) => {
                    tracing::error!(from, blake3 = %blake3_hex, error = %e, "collab get refused: serve check failed");
                    return Err(AbortReason::Permission);
                }
            };
            let decision = decide(rec.as_ref(), observed, serving, gauge.in_use(), limit);
            (decision, rec, Some(oracle))
        }
    };
    match decision {
        ServeDecision::Serve if !count_stream => Ok((None, rec)),
        ServeDecision::Serve => match gauge.try_acquire() {
            Some(permit) => Ok((Some(permit), rec)),
            // Lost the race for the last stream after the check said Serve.
            None => {
                tracing::debug!(
                    from,
                    streams = gauge.in_use(),
                    "collab get refused: upload stream limit"
                );
                Err(AbortReason::RateLimited)
            }
        },
        ServeDecision::RefuseLimit => {
            tracing::debug!(
                from,
                streams = gauge.in_use(),
                "collab get refused: upload stream limit"
            );
            Err(AbortReason::RateLimited)
        }
        ServeDecision::RefuseMismatch => {
            if let (Some(r), Some(o)) = (rec.as_ref(), oracle.as_ref()) {
                tracing::warn!(
                    from,
                    project_id = %r.project_id,
                    frame_uuid = %r.frame_uuid,
                    path = %r.path.display(),
                    "collab get refused: file changed on disk"
                );
                o.on_mismatch(r);
            }
            Err(AbortReason::Permission)
        }
        ServeDecision::RefuseNotHeld => {
            tracing::debug!(from, blake3 = %blake3_hex, "collab get refused: not a held current version");
            Err(AbortReason::Permission)
        }
        ServeDecision::RefuseUnavailable => {
            tracing::debug!(
                from,
                "collab get refused: collaboration storage unavailable"
            );
            Err(AbortReason::Permission)
        }
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
    /// Sender half of this endpoint's event stream; cloned into the control
    /// handler at construction. Retained here purely to keep the event channel
    /// open for the transport's lifetime (no longer read directly now that fetch
    /// progress flows through the `FetchSink` callback instead of this stream).
    #[allow(dead_code)]
    event_tx: mpsc::Sender<TransportEvent>,
    /// Receiver half, handed out once by [`events`](SharingTransport::events).
    event_rx: Mutex<Option<mpsc::Receiver<TransportEvent>>>,
    /// Connection-level authorization gate (collab exchange, slice 4). Cloned
    /// into both protocol handlers at construction; the host installs the
    /// predicate later via [`set_connect_gate`](Self::set_connect_gate).
    connect_gate: SharedConnectGate,
    /// Device-wide upload budget consulted by the provider throttle hook (W1).
    /// Constructed unlimited (rate 0) and re-rated live via
    /// [`set_upload_limit`](Self::set_upload_limit); the same `Arc` is held by the
    /// provider-events consumer, so a rate change takes effect on the next chunk
    /// of an in-flight transfer without rebinding anything.
    upload_pacer: Arc<UploadPacer>,
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

        // Legacy owned-endpoint transport: `EventSink::Direct` (single stream),
        // and `flush_store_on_shutdown: true` because the router teardown IS this
        // transport's only store flush (no separate `self.store.shutdown()`). The
        // responder is fixed for this transport's lifetime — wrapped in the shared
        // slot only so `SyncControlProtocol` has one responder type across both the
        // legacy transport and the shared node; `IrohTransport` never re-sets it.
        // The legacy single-stream transport is test-only and does not surface
        // serve-progress (production flows through the shared node's demux). Its
        // consumer still drains every request-update channel (SAFETY RULE) and its
        // Direct sink drops any ServeProgress, so it resolves nothing.
        let serve_resolver: ServeRootResolver = Arc::new(|_: Hash| -> Option<PackageId> { None });
        // The legacy single-stream transport surfaces no per-file progress either
        // (its Direct sink drops any `ServeFileProgress`); resolve nothing.
        let serve_file_resolver: ServeFileResolver =
            Arc::new(|_: Hash, _: u64| -> Option<(String, u64)> { None });
        // Unlimited until a host calls `set_upload_limit`; the consumer and this
        // struct share the one Arc so the limit is live-updatable.
        let upload_pacer = Arc::new(UploadPacer::new(0));
        let router = build_router(
            endpoint,
            &store,
            &connect_gate,
            EventSink::Direct(event_tx.clone()),
            Arc::new(Mutex::new(responder)),
            // The legacy single-stream transport is test-only and has no host to
            // install a presence hook; an empty slot means an inbound beacon is
            // acknowledged and dropped, which is the correct no-op here.
            Arc::new(Mutex::new(None)),
            true,
            serve_resolver,
            serve_file_resolver,
            Arc::clone(&upload_pacer),
            // The legacy transport serves no collab store.
            None,
        );

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
            upload_pacer,
        })
    }

    /// Set this endpoint's total sync UPLOAD limit in bytes/sec; `0` = unlimited.
    ///
    /// Takes effect on the next ~16 KiB chunk the provider offers — including
    /// mid-transfer — because the throttle hook holds this same pacer.
    pub fn set_upload_limit(&self, bytes_per_sec: u64) {
        self.upload_pacer.set_rate(bytes_per_sec);
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
        *self
            .connect_gate
            .lock()
            .expect("connect_gate mutex poisoned") = Some(gate);
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
    ///
    /// `router.shutdown()` already drives `GatedBlobs::shutdown` →
    /// `BlobsProtocol::shutdown` → `store.shutdown()` (flushing the persistent
    /// store and releasing its redb lock so a re-open over the same dir
    /// succeeds), so there is no separate `self.store.shutdown()` here — a second
    /// one would be a redundant shutdown of the same underlying store.
    pub async fn shutdown(self) {
        if let Err(e) = self.router.shutdown().await {
            tracing::debug!(error = %e, "iroh router shutdown");
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
            tx.write_all(&bytes)
                .await
                .context("write control message")?;
            tx.finish().context("finish control stream")?;
            // The receiver writes a one-byte ack only after handing the event to
            // the engine; an empty read means it closed without dispatching.
            let ack = rx
                .read_to_end(8)
                .await
                .context("await control delivery ack")?;
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
            tx.write_all(&bytes)
                .await
                .context("write control request")?;
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
                tracing::info!(
                    count = removed,
                    "blob store startup sweep removed stale tags"
                )
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

    async fn announce(
        &self,
        to: NodeId,
        a: &PackageAnnounce,
        batch_name: &str,
        batch_uuid: &str,
        files: &[AnnounceFileEntry],
        layout: PackageLayout,
    ) -> Result<()> {
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
        // Wire version follows the layout (mirror-hierarchy): only a Mirror
        // transfer needs the new field, so only it pays the compatibility cost.
        let msg = match layout {
            PackageLayout::Mirror => Msg::Announce4(PackageAnnounceV4 {
                package_id: wire.package_id,
                root_hash: wire.root_hash,
                byte_size: wire.byte_size,
                frame_count: wire.frame_count,
                batch_name: batch_name.to_string(),
                batch_uuid: batch_uuid.to_string(),
                files: files.to_vec(),
                layout,
            }),
            // Batch keeps the frozen v3 bytes — zero exposure for old peers.
            PackageLayout::Batch => Msg::Announce3(PackageAnnounceV3 {
                package_id: wire.package_id,
                root_hash: wire.root_hash,
                byte_size: wire.byte_size,
                frame_count: wire.frame_count,
                batch_name: batch_name.to_string(),
                batch_uuid: batch_uuid.to_string(),
                files: files.to_vec(),
            }),
        };
        self.send_control(to, msg).await?;
        tracing::debug!(
            to = %hex32(&to),
            package_id = %a.package_id.0,
            layout = layout.as_str(),
            "iroh announce sent"
        );
        Ok(())
    }

    async fn revoke(&self, to: NodeId, package_id: &PackageId, reason: RevokeReason) -> Result<()> {
        // One-shot best-effort control message (spec §D2): tell the receiver to
        // abort the transfer for `package_id`. `send_control` awaits the peer's
        // delivery ack; the caller (B3) log-and-continues on Err.
        self.send_control(
            to,
            Msg::Revoke {
                package_id: package_id.clone(),
                reason,
            },
        )
        .await?;
        tracing::debug!(to = %hex32(&to), package_id = %package_id.0, ?reason, "iroh revoke sent");
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

    async fn request_project(&self, to: NodeId, project_id: &str, package_id: &str) -> Result<()> {
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
        sink: FetchSink,
    ) -> Result<()> {
        let root_hash: Hash = pkg.root_hash.parse().with_context(|| {
            format!(
                "parse collection hash from announce root_hash {:?}",
                pkg.root_hash
            )
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
            pkg.byte_size,
            sink,
        )
        .await?;

        tracing::debug!(from = %hex32(&from), package_id = %pkg.package_id.0, "iroh fetch complete");
        Ok(())
    }

    async fn fetch_manifest(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
    ) -> Result<PathBuf> {
        let root_hash: Hash = pkg.root_hash.parse().with_context(|| {
            format!(
                "parse collection hash from announce root_hash {:?}",
                pkg.root_hash
            )
        })?;
        let provider =
            EndpointId::from_bytes(&from).map_err(|e| anyhow!("invalid provider node id: {e}"))?;
        blobs::fetch_manifest_to_dir(&self.store, &self.endpoint, provider, root_hash, dest_dir)
            .await
    }

    /// The legacy transport predates the `indexing` stage and has no sender engine
    /// waiting on it, so `_progress` is accepted and ignored (the shared node is the
    /// one that reports import bytes).
    async fn serve(
        &self,
        pkg: &PackageAnnounce,
        src_dir: &Path,
        want: Option<&HashSet<String>>,
        _progress: Option<ImportProgressSink>,
    ) -> Result<()> {
        let tag = package_tag(&pkg.package_id);
        // `None` → full package (pre-dedup). `Some(w)` → the negotiated subset:
        // only those payloads plus a manifest filtered to exactly them.
        // The legacy transport surfaces no per-file progress, so the ordered
        // entries are discarded here (only the shared node records them); it has
        // no `NodeOptions` either, so both paths keep the historic `Copy` import
        // (the shared node is the one that references payloads in place).
        let (hash, _entries) = match want {
            None => blobs::import_package_collection(&self.store, src_dir, &tag).await?,
            Some(w) => {
                blobs::import_subset_collection(
                    &self.store,
                    src_dir,
                    w,
                    &tag,
                    iroh_blobs::api::blobs::ImportMode::Copy,
                    None,
                )
                .await?
            }
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
        let tag = package_tag(package_id);
        let removed = self
            .store
            .tags()
            .delete(tag.as_bytes())
            .await
            .map_err(|e| anyhow!("delete package tag: {e}"))?;
        // Task 2.3 orphan hygiene: also drop the in-flight download tag (see
        // `blobs::fetch_collection_to_dir`). A terminal receiver outcome routes
        // through release, so this reclaims an in-flight tag left by a fetch that
        // errored/was cancelled without completing. Best-effort — never fail a
        // release on it; log first.
        let in_flight = blobs::in_flight_tag(&tag);
        if let Err(e) = self.store.tags().delete(in_flight.as_bytes()).await {
            tracing::warn!(
                package_id = %package_id.0,
                in_flight_tag = %in_flight,
                error = %format!("{e:#}"),
                "delete in-flight download tag on release failed"
            );
        }
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
            Msg::Want {
                want, candidates, ..
            } => (want, candidates),
            other => anyhow::bail!("expected Want reply to Offer, got {other:?}"),
        };

        let mut wanted: HashSet<String> = want.into_iter().collect();
        if candidates.is_empty() {
            tracing::debug!(to = %hex32(&to), package_id = %package_id.0, want = wanted.len(), "negotiate_want resolved (no candidates)");
            return Ok(wanted);
        }

        // Round 2: FullHashes → Want (still-wanted after full-hash disambiguation).
        let entries =
            proto::build_full_hash_entries(&offer, &candidates, &full_by_rel, &mut wanted);
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

/// Where a control-accept handler routes a decoded inbound [`TransportEvent`],
/// and how the accept loop should react (whether to send the transport-level
/// delivery ack). Two shapes coexist while [`IrohTransport`] and the
/// [`SharedIrohNode`](node::SharedIrohNode) both live (Task 3 removes the former):
///
/// - [`Direct`](EventSink::Direct): the legacy single-stream transport — one
///   channel, ack on a successful send, stop on a gone consumer.
/// - [`Demux`](EventSink::Demux): the shared node's per-`(peer, package)`
///   ack-claim + single Recv-consumer router (Д4), which additionally
///   distinguishes an *orphan* (no claim/consumer) so the accept loop withholds
///   the delivery ack and the sender retries instead of losing the message.
#[derive(Clone)]
pub(crate) enum EventSink {
    Direct(mpsc::Sender<TransportEvent>),
    Demux(Arc<node::EventDemux>),
}

/// The outcome of routing one inbound control event through an [`EventSink`],
/// telling [`SyncControlProtocol::accept`] whether to send the delivery ack.
pub(crate) enum Delivery {
    /// A consumer received the event — send the transport-level delivery ack.
    Delivered,
    /// No claim/consumer for this event — drop it and withhold the ack so the
    /// sender retries (the demux logs the orphan).
    Orphan,
    /// The routed consumer's receiver is gone — withhold the ack and stop
    /// accepting on this connection (legacy single-stream semantics).
    ConsumerGone,
}

impl EventSink {
    /// Route one decoded inbound event to its consumer, returning how the accept
    /// loop should proceed.
    async fn deliver(&self, event: TransportEvent) -> Delivery {
        match self {
            EventSink::Direct(tx) => match tx.send(event).await {
                Ok(()) => Delivery::Delivered,
                Err(_) => Delivery::ConsumerGone,
            },
            EventSink::Demux(demux) => demux.deliver_inbound(event).await,
        }
    }

    /// Route a locally-generated [`ServeProgress`](TransportEvent::ServeProgress)
    /// (from the provider-upload-events consumer, Task 13) to the sender engine.
    /// Non-blocking (`try_send`): a full or absent consumer drops the tick, which
    /// is CORRECT — upload progress is best-effort UI data, and blocking here would
    /// stall the consumer that must keep draining request updates for the transfer.
    ///
    /// - [`Direct`](EventSink::Direct): the legacy single-stream transport is
    ///   test-only and does NOT surface serve-progress — folding it into the shared
    ///   announce/ack stream is the exact misrouting hazard the demux exists to
    ///   avoid (audit C1). No-op; production flows through `Demux` below.
    /// - [`Demux`](EventSink::Demux): the ack-claim owner(s) for this `package_id`
    ///   (the Out/Collab handle that announced it); a package with no live claim is
    ///   dropped.
    fn route_serve_progress(&self, package_id: &PackageId, bytes_sent: u64) {
        match self {
            EventSink::Direct(_) => {}
            EventSink::Demux(demux) => demux.route_serve_progress(package_id, bytes_sent),
        }
    }

    /// Route a locally-generated [`ServeComplete`](TransportEvent::ServeComplete)
    /// (Task 2.1) — the terminal-success sibling of
    /// [`route_serve_progress`](Self::route_serve_progress), fired from the
    /// provider-events consumer on the `Completed` of a payload-carrying request.
    /// Same best-effort `try_send` semantics and the same Direct/Demux split: the
    /// legacy single-stream transport (test-only) is a no-op; production routes
    /// through the demux to the ack-claim owner(s) for this `package_id`.
    fn route_serve_complete(&self, package_id: &PackageId) {
        match self {
            EventSink::Direct(_) => {}
            EventSink::Demux(demux) => demux.route_serve_complete(package_id),
        }
    }

    /// Route a locally-generated
    /// [`ServeFileProgress`](TransportEvent::ServeFileProgress) (Task 2.2) — the
    /// per-file sibling of [`route_serve_progress`](Self::route_serve_progress),
    /// fired from the provider-events consumer as it attributes a served child to
    /// its collection entry by index. Same best-effort `try_send` semantics and the
    /// same Direct/Demux split: the legacy single-stream transport (test-only) is a
    /// no-op; production routes through the demux to the ack-claim owner(s) for this
    /// `package_id`.
    fn route_serve_file_progress(
        &self,
        package_id: &PackageId,
        file: String,
        bytes_done: u64,
        bytes_total: u64,
    ) {
        match self {
            EventSink::Direct(_) => {}
            EventSink::Demux(demux) => {
                demux.route_serve_file_progress(package_id, file, bytes_done, bytes_total)
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
    /// Where decoded inbound `Announce`/`Ack`/`Project*` events are routed
    /// (legacy single stream, or the shared node's demux — see [`EventSink`]).
    sink: EventSink,
    /// Fired when a peer announces its presence (D1), in a late-bindable shared
    /// slot like [`responder`](Self::responder) — the host installs it after the
    /// router is spawned. Empty ⇒ the beacon is acknowledged and dropped at
    /// debug, which is what a send-only endpoint or a not-yet-wired host wants.
    presence: SharedPresenceHook,
    /// Answers the dedup handshake, in a late-bindable shared slot (so the shared
    /// node can install it after the router is spawned). Empty ⇒ a send-only
    /// endpoint or a peer with no catalog wired, in which case offers are
    /// answered want-all.
    responder: SharedResponder,
    /// Connection-level authorization gate (slice 4). Checked once, at the top of
    /// `accept`, before any `Msg` is decoded. Absent ⇒ admit (accept-all).
    gate: SharedConnectGate,
}

// `dyn DedupResponder` isn't `Debug`, so the derive can't apply; the iroh
// `ProtocolHandler` bound requires `Debug`, so hand-roll a minimal one.
impl std::fmt::Debug for SyncControlProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncControlProtocol")
            .field(
                "has_responder",
                &self.responder.lock().map(|r| r.is_some()).unwrap_or(false),
            )
            .finish()
    }
}

impl ProtocolHandler for SyncControlProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        // Gate placement (design decision Д1, iroh 1.0.2 verified 2026-07-15):
        // the authenticated peer id first exists here, on the completed
        // `Connection` (`remote_id()`). iroh's pre-handshake stages —
        // `Incoming`/`Accepting` (`connection.rs`) — expose ONLY socket
        // addresses, no identity, so an `incoming_filter`/`before_connect` gate
        // could not authorize by node id. The handler-level gate therefore stays
        // exactly where the remote identity becomes available; the residual
        // pre-handshake DoS surface is the documented, accepted trade-off.
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
                // Announce v1 (e.g. Perseus beta.3) / v2 / v3 / v4 →
                // AnnounceReceived. The shared mapping splits the manifest extras
                // off the wire struct and applies the batch-identity fallback
                // (v1/v2 → wire package id) plus the layout fallback (v1/v2/v3 →
                // Batch), so downstream code is version-agnostic.
                m @ (Msg::Announce(_)
                | Msg::Announce2(_)
                | Msg::Announce3(_)
                | Msg::Announce4(_)) => announce_received_from_msg(from, m),
                Msg::Ack {
                    package_id,
                    receipts,
                } => TransportEvent::AckReceived {
                    from,
                    package_id,
                    receipts,
                },
                // Sender revoked an outstanding announce (spec §D2): forward it as
                // an in-process event, then the b"1" delivery ack — the same
                // deliver-then-ack shape as Announce/Ack. No consumer wires the
                // abort yet (B4); the receiver loop logs + ignores it.
                Msg::Revoke { package_id, reason } => TransportEvent::RevokeReceived {
                    from,
                    package_id,
                    reason,
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
                    let responder = self
                        .responder
                        .lock()
                        .expect("responder mutex poisoned")
                        .clone();
                    let (want, candidates) = match responder {
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
                    let responder = self
                        .responder
                        .lock()
                        .expect("responder mutex poisoned")
                        .clone();
                    let still = match responder {
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
                // Peer reachability (D1): a presence beacon is not a package event
                // — it never enters the demux (there is no package to claim and no
                // Recv consumer to route to), so it fires the host hook directly
                // and is acknowledged like any other control message. With no hook
                // installed it is a deliberate no-op rather than an error: a
                // send-only endpoint, or a host that has not wired up yet, must
                // never tell a well-behaved peer its beacon failed.
                Msg::Presence => {
                    let hook = self
                        .presence
                        .lock()
                        .expect("presence hook mutex poisoned")
                        .clone();
                    match hook {
                        Some(h) => {
                            tracing::debug!(from = %hex32(&from), "peer presence received");
                            h(from);
                        }
                        None => tracing::debug!(
                            from = %hex32(&from),
                            "peer presence received; no hook installed"
                        ),
                    }
                    let _ = tx.write_all(b"1").await;
                    let _ = tx.finish();
                    continue;
                }
            };
            // Deliver in-process, then ack IFF a consumer received it. The
            // shared node's demux routes by `(peer, package)` ack-claim / Recv
            // consumer; an orphan (no claim/consumer) is dropped WITHOUT the
            // transport-level delivery ack so the sender retries instead of
            // losing the message silently (audit C1 / Task 2).
            match self.sink.deliver(event).await {
                Delivery::Delivered => {
                    let _ = tx.write_all(b"1").await;
                    let _ = tx.finish();
                }
                Delivery::Orphan => {
                    // No delivery ack (the demux already warned): leave the
                    // sender's read unanswered so it errors/times out and retries.
                }
                Delivery::ConsumerGone => {
                    tracing::debug!(from = %hex32(&from), "control event consumer gone; closing");
                    break;
                }
            }
        }
        Ok(())
    }
}

/// A thin gating wrapper around the iroh-blobs [`ProtocolHandler`] (collab
/// exchange, slice 4). It evaluates the shared connect gate against the dialing
/// peer's node id and only then delegates to the inner `iroh_blobs` handler, so
/// an ungated peer never receives a single blob byte (finding F5 hardening).
/// With no gate installed it delegates unconditionally — today's behavior.
pub(crate) struct GatedBlobs {
    inner: BlobsProtocol,
    gate: SharedConnectGate,
    /// Whether a router shutdown should flush the backing blob store (iroh
    /// hardening T8). `true` for the legacy [`IrohTransport`], whose only store
    /// flush IS this router-driven path. `false` for the [`SharedIrohNode`], whose
    /// store is SHARED across relay rebuilds: its per-rebuild router teardown must
    /// NOT tear the store down (the accept loop calls `protocols.shutdown()` when
    /// its endpoint closes), so the node flushes the store explicitly in its own
    /// [`shutdown`](crate::sharing::iroh::node::SharedIrohNode::shutdown) instead.
    flush_store_on_shutdown: bool,
    /// This provider's accepted connections (peer naming in the provider
    /// consumer; for the collab store also the I11 close, P16).
    conns: Arc<ConnRegistry>,
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
        // Gate placement per Д1 (see `SyncControlProtocol::accept`): `remote_id()`
        // on the completed `Connection` is the earliest point iroh 1.0.2 exposes
        // the authenticated peer id, so the blobs gate lives here too.
        let from: NodeId = *connection.remote_id().as_bytes();
        if !connect_gate_admits(&self.gate, &from) {
            tracing::warn!(from = %hex32(&from), "connection refused by connect gate");
            connection.close(0u32.into(), b"unauthorized");
            return Ok(());
        }
        if !self.conns.register_admitted(&connection, &self.gate) {
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
    // and the default (no-op) would otherwise silently drop that flush. The
    // `SharedIrohNode` opts OUT (`flush_store_on_shutdown = false`): its store is
    // shared across relay rebuilds, so a per-rebuild router teardown must not tear
    // it down — the node flushes it explicitly at its own shutdown (T8).
    async fn shutdown(&self) {
        if self.flush_store_on_shutdown {
            <BlobsProtocol as ProtocolHandler>::shutdown(&self.inner).await
        }
    }
}

/// The collab store currently mounted on the shared node (collab v3 wave 2,
/// P1/R4): the store handle and the unchanged [`GatedBlobs`] provider that
/// serves it. Lives in a [`SharedCollabSlot`]; swapped whole on a
/// Collaboration-root change.
pub(crate) struct CollabMount {
    /// The Collaboration root this store was opened under (the store itself is
    /// `<root>/.athenaeum/blobs`).
    pub(crate) root: PathBuf,
    pub(crate) store: Store,
    pub(crate) blobs: Arc<GatedBlobs>,
    /// Ends the store's GC task once the store is shut down (Task 15 R4).
    pub(crate) gc: node::StoreGc,
}

/// The node's collab-store slot. `None` ⇒ no Collaboration root mounted, and
/// [`CollabSlotBlobs`] refuses every connection. Never held across an await:
/// readers clone what they need out.
pub(crate) type SharedCollabSlot = Arc<RwLock<Option<CollabMount>>>;

/// Build the provider for a freshly opened collab store: stock
/// [`BlobsProtocol`] fed by the node's collab event sender (whose consumer shares
/// the device-wide pacer), wrapped in the same [`GatedBlobs`] as the personal
/// store. `flush_store_on_shutdown: false` — the node shuts the collab store
/// down itself (on a swap and at node shutdown), never a router teardown.
pub(crate) fn collab_gated_blobs(
    store: &Store,
    events: EventSender,
    gate: &SharedConnectGate,
    conns: &Arc<ConnRegistry>,
) -> GatedBlobs {
    GatedBlobs {
        inner: BlobsProtocol::new(store, Some(events)),
        gate: Arc::clone(gate),
        flush_store_on_shutdown: false,
        conns: Arc::clone(conns),
    }
}

/// The [`COLLAB_BLOBS_ALPN`] handler, mounted ONCE at bind (ruling R4). A router
/// cannot add protocols after spawn, and rebuilding one would close the endpoint
/// or abort every live personal-sync handler, so this handler reads the node's
/// [`SharedCollabSlot`] per connection instead: the connect gate first (a refused
/// peer is refused whether or not a store is mounted), then the mounted store's
/// [`GatedBlobs`], or a close with `not mounted` when the slot is empty.
///
/// A connection accepted before a swap keeps the provider it started with; its
/// later requests fail once the old store is shut down, and the peer retries.
pub(crate) struct CollabSlotBlobs {
    pub(crate) slot: SharedCollabSlot,
    pub(crate) gate: SharedConnectGate,
}

impl std::fmt::Debug for CollabSlotBlobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CollabSlotBlobs").finish()
    }
}

impl ProtocolHandler for CollabSlotBlobs {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        let from: NodeId = *connection.remote_id().as_bytes();
        if !connect_gate_admits(&self.gate, &from) {
            tracing::warn!(from = %hex32(&from), "collab connection refused by connect gate");
            connection.close(0u32.into(), b"unauthorized");
            return Ok(());
        }
        let blobs = match self.slot.read() {
            Ok(guard) => guard.as_ref().map(|m| Arc::clone(&m.blobs)),
            Err(e) => {
                tracing::error!(error = %e, "collab store slot poisoned; refusing connection");
                None
            }
        };
        match blobs {
            Some(blobs) => <GatedBlobs as ProtocolHandler>::accept(&blobs, connection).await,
            None => {
                tracing::debug!(from = %hex32(&from), "collab connection refused: store not mounted");
                connection.close(0u32.into(), b"collab store not mounted");
                Ok(())
            }
        }
    }
    // `shutdown` stays the default no-op: the node flushes the collab store
    // itself, and a mounted provider never owns the store's lifetime.
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
