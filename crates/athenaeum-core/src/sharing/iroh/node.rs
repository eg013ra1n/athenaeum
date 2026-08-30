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
//! that module's construction shape, protocol handlers
//! ([`SyncControlProtocol`](super::SyncControlProtocol),
//! [`GatedBlobs`](super::GatedBlobs)) — mounted via the shared
//! [`build_router`](super::build_router) constructor — blob glue ([`blobs`]), and
//! connection-path diagnostics
//! verbatim, adding: a process-lifetime advisory **lock** on the device-key file
//! (I4), **role-prefixed** blob tags with a **prefix-scoped** startup sweep (Д3,
//! so one role's sweep can't wipe another's live tags on the shared store), a
//! **home-relay status** watcher, and an idempotent, bounded **shutdown** (I1).
//! `IrohTransport` stays alive as the loopback/test engine until Task 3 migrates
//! every production call site onto this node.
//!
//! Event demux and the pooled control connection are **Task 2** and now live
//! here: a per-`(peer, package)` ack-claim + single Recv-consumer [`EventDemux`]
//! (Д4) routes each inbound event to exactly one consumer (never the ambiguous
//! shared stream that let a misrouted ack be silently dropped, audit C1), and a
//! per-peer [`ControlPool`] reuses one idle-closed QUIC connection per peer
//! instead of dialing per control message.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use iroh::endpoint::{presets, Connection};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey, Watcher};
use iroh_blobs::api::blobs::ImportMode;
use iroh_blobs::api::Store;
use iroh_blobs::store::fs::options::Options as FsOptions;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::store::GcConfig;
use iroh_blobs::Hash;
use iroh_tickets::endpoint::EndpointTicket;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

use crate::account::keys::{device_key_lock_path, DeviceKey, DeviceKeyLock};
use crate::sharing::types::{
    AnnounceFileEntry, FrameReceipt, NodeId, PackageAnnounce, PackageAnnounceV3, PackageAnnounceV4,
    PackageId, PackageLayout, RevokeReason, StartInfo, TransportEvent,
};
use crate::sharing::{FetchSink, ProviderTelemetrySink, SharingTransport};
use crate::sync::status::TransportHealth;
use crate::sync::DedupResponder;

use super::pacer::UploadPacer;
use super::proto::{self, Msg, OfferEntry};
use super::telemetry::{peer_conn_path, TransportCounters};
use super::{
    blobs, build_router, hex32, spawn_conn_path_diagnostics, ConnectGate, Delivery, EventSink,
    PresenceHook, ServeFileResolver, ServeRootResolver, SharedConnectGate, SharedPresenceHook,
    SharedResponder, CONTROL_SEND_TIMEOUT, EVENT_CHANNEL_CAPACITY, GC_INTERVAL, MAX_CONTROL_BYTES,
    ONLINE_TIMEOUT, SYNC_ALPN,
};

/// Upper bound on the graceful `endpoint.close()` at shutdown (I1). A clean
/// close lets peers see a QUIC close instead of a reset and clears the relay
/// registration promptly; if it stalls we log and move on rather than hang exit.
const SHUTDOWN_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Cap on one presence beacon (connect + write + delivery ack). Deliberately far
/// shorter than [`CONTROL_SEND_TIMEOUT`]: a beacon fan-out runs across every
/// account device at startup, and a peer that is switched off must not make the
/// live ones wait behind it.
const PRESENCE_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the transport-telemetry sampler logs a traffic window. Long on
/// purpose: these are cumulative counters, so a coarse tick loses nothing but
/// keeps an all-day session down to a handful of lines — and a window with no
/// traffic and no re-home is skipped entirely, so an idle app stays silent.
const TELEMETRY_INTERVAL: Duration = Duration::from_secs(300);

/// Threshold (ms) above which handing a decoded inbound event to its consumer's
/// channel is logged as delayed (Task 4.1, candidate (a) — a receive stalling
/// behind a send). A `tx.send().await` that blocks this long means the bounded
/// consumer channel stayed full because the receiver loop was busy (e.g.
/// processing a prior announce inline). Instrumentation only — no behavior change.
const SLOW_INBOUND_DELIVERY_MS: u128 = 1000;

/// Idle window before a pooled control connection is closed and evicted (Task 2).
/// Long enough that a burst of announces/acks reuses one connection; short enough
/// that a peer gone quiet doesn't hold an endpoint slot indefinitely.
const CONTROL_POOL_IDLE: Duration = Duration::from_secs(60);

/// After a probe's control connection establishes, how long to watch for a
/// peer-initiated close before calling the holder reachable (Task 9). A refusing
/// [`ConnectGate`] closes the fresh connection within milliseconds
/// (`close(0, "unauthorized")`), so a short window cleanly separates a
/// *refused* holder (closed) from a *reachable* one (stays open); a reachable
/// holder always waits the full window.
const PROBE_CLOSE_WINDOW: Duration = Duration::from_secs(1);

/// Why a short holder-reachability probe failed (Task 9), recorded per holder in
/// the download orchestrator's transfer-history detail. Best-effort classification
/// of a control-connect probe outcome: the real network can only ever be
/// approximated here, so a class is a diagnostic hint, never an authorization
/// signal (S5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeClass {
    /// No route / no addressing info (or a dial timeout with no relay hint) — the
    /// holder looks offline.
    Offline,
    /// The control connection established but the peer refused it (its
    /// [`ConnectGate`] closed the connection) — the holder is up but declined us.
    Refused,
    /// A dial timeout with a relay hint present — the relay path is unreachable.
    RelayUnreachable,
}

impl ProbeClass {
    /// Stable lowercase tag for the transfer-history detail / log field.
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeClass::Offline => "offline",
            ProbeClass::Refused => "refused",
            ProbeClass::RelayUnreachable => "relay_unreachable",
        }
    }
}

/// Classify a *failed* control-dial (the `connect()` returned an error) into a
/// [`ProbeClass`] (Task 9). Pure over the error text + whether a relay hint was
/// present, so it is unit-testable without a network. A peer that refused the
/// connection surfaces as an application close ("unauthorized"/"closed by peer");
/// a relay-only route that timed out is `RelayUnreachable`; anything else (no
/// addressing information, no route) is `Offline`.
fn classify_connect_err(msg: &str, has_relay_hint: bool) -> ProbeClass {
    let m = msg.to_lowercase();
    if m.contains("unauthorized")
        || m.contains("closed by peer")
        || m.contains("application")
        || m.contains("refused")
        || m.contains("forbidden")
    {
        ProbeClass::Refused
    } else if has_relay_hint && (m.contains("timed out") || m.contains("timeout")) {
        ProbeClass::RelayUnreachable
    } else {
        ProbeClass::Offline
    }
}

/// How often the relay-map refresh loop re-runs its resolver (H2, Task 8). Hourly
/// is a slack cadence — a relay-map change is rare and the sign-in path triggers
/// an immediate refresh out of band ([`SharedIrohNode::request_relay_refresh`]).
const RELAY_REFRESH_INTERVAL: Duration = Duration::from_secs(3600);

/// A `Send` boxed future — the return type of the injected relay resolver and (in
/// the engine) the retry address refresher. Defined locally so no `futures`/
/// `n0-future` boxing type leaks into the node's public signature.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// The host-injected, hub-agnostic relay-map resolver the refresh loop re-runs
/// (H2, Task 8). Returns the CURRENT `(RelayMode, relay_urls)` the transport
/// should be on — the app re-runs `resolve_relay_mode`, Perseus re-runs its relay
/// resolution — or `None` when it can't resolve right now (hub blip), in which
/// case the loop keeps the existing relay map. The node stays hub-agnostic: it
/// never reaches the hub itself, it only calls this callback.
pub type RelayResolver = Arc<dyn Fn() -> BoxFuture<Option<(RelayMode, Vec<String>)>> + Send + Sync>;

/// A transport-level wake hook (Task 6, sync delivery-forever). Invoked when the
/// node comes back online — a home-relay **reconnect** transition in the watcher,
/// or an **applied relay-map change** in the refresh loop — so the host can kick
/// every pending outbound package out of its backoff at once (the api layer's
/// closure fans a `SyncSenderRuntime::kick_all` over the personal + collab sender
/// maps). Installed via [`SharedIrohNode::set_wake_hook`]; `None` until then, so
/// every fire before install is a silent no-op. Stored behind an `Arc<RwLock<…>>`
/// so the home-relay watcher task — spawned at bind, before the host installs the
/// hook — reads the CURRENT hook at fire time rather than a stale clone at spawn.
pub type WakeHook = Arc<dyn Fn() + Send + Sync>;

/// The node's current home-relay connectivity (Task 3.3), read by
/// [`SharedIrohNode::transport_health`] on the status poll. Held behind an
/// `Arc<RwLock<…>>` on the OUTER layer of the node (like [`WakeHook`]) — the
/// watcher writes into this cell and a status poll reads it, so a poll always
/// sees the current picture. The initial value is disconnected; the successful
/// one-shot `online()` wait seeds it connected as a bridge, and the watcher
/// keeps it current — so it is the sole connected-ness authority and a dropped
/// relay flips the surface back to `direct_only`.
///
/// **Aggregate, not last-transition.** The watcher recomputes this from the WHOLE
/// home-relay status set (connected iff ANY relay is connected — the rule
/// [`iroh::Endpoint::online`] uses), because iroh 1.x can hold more than one home
/// relay and does re-home between them. Writing the cell from whichever entry
/// changed last made a secondary relay's drop report `direct_only` while another
/// relay was up.
#[derive(Debug, Clone)]
pub struct RelayHealth {
    /// Whether ANY home relay is currently connected.
    pub connected: bool,
    /// The connected relay's URL when [`connected`](Self::connected); otherwise
    /// the URL of the relay whose failure is reported in
    /// [`last_error`](Self::last_error), if any is known.
    pub url: Option<String>,
    /// The relay error explaining the current disconnected state, if any.
    pub last_error: Option<String>,
    /// When this snapshot was recorded — only advanced when the picture actually
    /// changes, so it reads as "in this state since".
    pub since: Instant,
}

impl RelayHealth {
    /// The pre-transition disconnected baseline the node binds with.
    fn initial() -> Self {
        Self {
            connected: false,
            url: None,
            last_error: None,
            since: Instant::now(),
        }
    }
}

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

/// The tag-store prefix under which the RECEIVER's in-flight download tags live:
/// `in-flight/recv/pkg/`. In-flight tags are receiver-only — a fetch (their only
/// writer, [`blobs::in_flight_tag`]) runs on the [`Role::Recv`] handle — so this
/// is the one namespace the B7 orphan sweep enumerates. Kept as a single literal
/// and pinned against the tag constructors ([`blobs::in_flight_tag`] +
/// [`role_package_tag`]) by `recv_in_flight_prefix_matches_constructors` so the
/// sweep and the writer can never drift.
const RECV_IN_FLIGHT_PREFIX: &str = "in-flight/recv/pkg/";

/// The tag namespace under which PROJECT SEEDING (D3 §3.4) pins collections:
/// `project/<project_id>/<package_id>`. Two producers write here — a publisher
/// seeding what it just published ([`SharedIrohNode::seed_project_collection`]
/// from `api::collab::publish_collab_frames`, D3 T2) and a downloader seeding
/// what it just ingested (the same call from `api::collab_exchange`, D3 T4) —
/// and the only deleters are [`SharedIrohNode::unseed_project_package`] /
/// [`SharedIrohNode::unseed_project`], driven by local-data deletion.
///
/// The namespace is deliberately outside BOTH transfer-machinery scopes: the
/// per-role startup sweep only deletes `<prefix>/pkg/` ([`SharedIrohNode::role_start`])
/// and the B7 orphan reclaim only enumerates [`RECV_IN_FLIGHT_PREFIX`]
/// ([`recv_in_flight_package_id`]), so a seed survives every boot until its
/// project's data is deleted. Seeds must be durable that way: a swept seeding tag
/// would turn a real holder into a phantom the hub still advertises.
const PROJECT_SEED_PREFIX: &str = "project/";

/// The seeding tag for ONE project package (D3 §3.4).
fn project_seed_tag(project_id: &str, package_id: &str) -> String {
    format!("{PROJECT_SEED_PREFIX}{project_id}/{package_id}")
}

/// The tag prefix covering EVERY package seeded for one project — the delete
/// scope of [`SharedIrohNode::unseed_project`].
fn project_seed_prefix(project_id: &str) -> String {
    format!("{PROJECT_SEED_PREFIX}{project_id}/")
}

/// If `tag` is a receiver-side in-flight download tag
/// (`in-flight/recv/pkg/<wire_package_id>`), return that wire package id; else
/// `None`. This one function IS the B7 namespace contract: a permanent
/// `<role>/pkg/…` tag, a `project/…` seeding tag ([`PROJECT_SEED_PREFIX`]), or
/// any other foreign tag yields `None`, so the orphan sweep that releases what
/// this matches can never touch a tag the transfer machinery does not own.
/// Package ids are single path-segment uuids/hex, so a value carrying a `/` is
/// rejected as malformed.
fn recv_in_flight_package_id(tag: &str) -> Option<&str> {
    tag.strip_prefix(RECV_IN_FLIGHT_PREFIX)
        .filter(|id| !id.is_empty() && !id.contains('/'))
}

/// Resolve a served collection's root hash back to the [`PackageId`] whose
/// [`serve`](SharingTransport::serve) registered it (Task 13), by scanning the
/// role-prefixed `served` map (`<prefix>/pkg/<package_id>` → hash). The package id
/// is the segment after `/pkg/`. `None` for a child blob / hash-seq-internal /
/// foreign hash — the provider-events consumer still drains those, it just emits
/// no progress for them. Backs both [`SharedIrohNode::resolve_served_root`] and the
/// [`ServeRootResolver`] closure the consumer holds.
/// What a `serve` recorded for one package tag: the imported collection's root
/// hash plus the fingerprint of the want-subset it was built from.
///
/// The fingerprint is what makes the re-serve short-circuit safe (D1 §3.5): two
/// serves of the same package with DIFFERENT subsets are different collections,
/// so reusing the first hash for the second would advertise a collection missing
/// the frames the peer just asked for.
#[derive(Debug, Clone)]
struct ServedCollection {
    hash: Hash,
    want_fingerprint: Option<String>,
    /// The on-disk dir this collection was imported from — `packages/<uuid>` for a
    /// role serve, the retained package dir for a project seed. Under
    /// [`ImportMode::TryReference`] the store REFERENCES files under here, so
    /// `src_dir.join(rel_path)` is where a child's bytes actually live; that is
    /// what lets [`copy_shared_children_before_release`](SharedIrohNode::copy_shared_children_before_release)
    /// find a live source for a blob another package still needs.
    src_dir: PathBuf,
}

/// Stable fingerprint of a serve's want-subset: `None` for a full package, else
/// the sorted rel_paths joined. Sorted because the subset arrives as a `HashSet`,
/// whose iteration order would otherwise make an identical subset look different.
fn want_fingerprint(want: Option<&HashSet<String>>) -> Option<String> {
    want.map(|w| {
        let mut v: Vec<&str> = w.iter().map(String::as_str).collect();
        v.sort_unstable();
        v.join("\n")
    })
}

fn resolve_served_root_in(
    served: &Mutex<HashMap<String, ServedCollection>>,
    hash: Hash,
) -> Option<PackageId> {
    let map = served.lock().expect("served mutex poisoned");
    map.iter().find_map(|(tag, entry)| {
        if entry.hash == hash {
            tag.split("/pkg/")
                .nth(1)
                .map(|id| PackageId(id.to_string()))
        } else {
            None
        }
    })
}

/// Resolve a served collection's `(root hash, hash-seq index)` to the collection
/// ENTRY at that index (Task 2.2) — `index-2 → entries[index-2]`, returning its
/// `(rel_path, byte_size)`. Indices 0 (root hash-seq blob) and 1 (iroh
/// `CollectionMeta`) resolve `None` (they carry no payload entry). Attribution is
/// STRICTLY by index: the root hash locates the serving tag (whose `served_files`
/// row holds the entries in collection order — the SUBSET order for a want-subset
/// serve, since that is what the provider iterates), then the index selects the
/// entry, so two byte-identical files (one blob hash, two entries) each resolve to
/// their own `rel_path`. Backs the [`ServeFileResolver`] closure the consumer
/// holds; spawned before `Self` exists, so it works over the shared maps directly.
fn resolve_served_file_in(
    served: &Mutex<HashMap<String, ServedCollection>>,
    served_files: &Mutex<HashMap<String, Vec<(String, u64)>>>,
    root: Hash,
    hash_seq_index: u64,
) -> Option<(String, u64)> {
    // Offsets 0 (root hash-seq) and 1 (CollectionMeta) are not payload entries.
    let entry_idx = usize::try_from(hash_seq_index.checked_sub(2)?).ok()?;
    // Find the tag serving this root hash (the by-hash lookup is ONLY to locate
    // the served package's entry list — the entry itself is then chosen by index).
    let tag = {
        let map = served.lock().expect("served mutex poisoned");
        map.iter()
            .find_map(|(tag, entry)| (entry.hash == root).then(|| tag.clone()))
    }?;
    let files = served_files.lock().expect("served_files mutex poisoned");
    files
        .get(&tag)
        .and_then(|entries| entries.get(entry_idx).cloned())
}

/// Per-`(peer, package)` ack-claim + single Recv-consumer router (Task 2, Д4).
///
/// The shared node owns ONE inbound event stream (the control-protocol accept
/// path). Before this, every role handle shared that stream, so an ack could be
/// delivered to the wrong sender engine which silently dropped it (audit C1). The
/// demux instead routes each decoded inbound event to exactly one consumer:
///
/// - `AnnounceReceived` / `Project*Received` → the single registered **Recv**
///   consumer (`handle(Role::Recv).events()`).
/// - `AckReceived { from, package_id }` → the **claimant** that registered
///   `(from, package_id)` when it announced (an Out/Collab handle's own channel).
///
/// An event with no matching claim/consumer is an **orphan**: logged and dropped
/// WITHOUT a delivery ack (so the sender retries), never misrouted. Claims are
/// released on the routed ack, an announce failure, or the owning handle's drop.
pub(crate) struct EventDemux {
    inner: Mutex<DemuxInner>,
}

#[derive(Default)]
struct DemuxInner {
    /// The single registered Recv consumer: `(owning handle id, sender)`.
    recv: Option<(u64, mpsc::Sender<TransportEvent>)>,
    /// Ack claims: `(peer, package_id)` → `(owning handle id, that handle's sender)`.
    claims: HashMap<(NodeId, PackageId), (u64, mpsc::Sender<TransportEvent>)>,
}

impl EventDemux {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(DemuxInner::default()),
        })
    }

    /// Register (or replace) the single Recv consumer. Only one Recv handle runs
    /// in practice; a later registration wins (last-consumer-wins).
    fn register_recv(&self, handle_id: u64, tx: mpsc::Sender<TransportEvent>) {
        self.inner.lock().expect("demux mutex poisoned").recv = Some((handle_id, tx));
    }

    /// Register an ack claim for `(peer, package_id)` pointing at the announcing
    /// handle's own events channel.
    fn register_claim(
        &self,
        handle_id: u64,
        key: (NodeId, PackageId),
        tx: mpsc::Sender<TransportEvent>,
    ) {
        self.inner
            .lock()
            .expect("demux mutex poisoned")
            .claims
            .insert(key, (handle_id, tx));
    }

    /// Release a single ack claim (announce-failure path; the delivery path
    /// consumes its own claim inside [`deliver_inbound`](Self::deliver_inbound)).
    fn release_claim(&self, key: &(NodeId, PackageId)) {
        self.inner
            .lock()
            .expect("demux mutex poisoned")
            .claims
            .remove(key);
    }

    /// Drop every registration owned by a handle that is going away (its `Drop`),
    /// so a claim/consumer can never outlive its handle and leak.
    fn release_handle(&self, handle_id: u64) {
        let mut inner = self.inner.lock().expect("demux mutex poisoned");
        inner.claims.retain(|_, (owner, _)| *owner != handle_id);
        if matches!(inner.recv, Some((id, _)) if id == handle_id) {
            inner.recv = None;
        }
    }

    /// Route one decoded inbound accept-path event to its consumer, returning how
    /// the accept loop should proceed (see [`Delivery`]). Called from the shared
    /// [`EventSink::Demux`] arm in the parent module.
    pub(crate) async fn deliver_inbound(&self, event: TransportEvent) -> Delivery {
        // Resolve the target sender under the lock (consuming an ack's claim, so a
        // claim is released the moment its ack routes), then send without the lock.
        let target = {
            let mut inner = self.inner.lock().expect("demux mutex poisoned");
            match &event {
                TransportEvent::AckReceived {
                    from, package_id, ..
                } => inner
                    .claims
                    .remove(&(*from, package_id.clone()))
                    .map(|(_, tx)| tx),
                TransportEvent::AnnounceReceived { .. }
                | TransportEvent::RevokeReceived { .. }
                | TransportEvent::ProjectAnnounceReceived { .. }
                | TransportEvent::ProjectRequestReceived { .. } => {
                    inner.recv.as_ref().map(|(_, tx)| tx.clone())
                }
                // ServeProgress / ServeComplete / ServeFileProgress originate on OUR
                // endpoint (the provider-events consumer), never arrive as a decoded
                // inbound control message, and are routed via `route_serve_progress`
                // / `route_serve_complete` / `route_serve_file_progress` — not this
                // path. Treat a stray one defensively as an orphan (no consumer, no
                // delivery ack).
                TransportEvent::ServeProgress { .. }
                | TransportEvent::ServeComplete { .. }
                | TransportEvent::ServeFileProgress { .. } => None,
            }
        };
        match target {
            Some(tx) => {
                // Task 4.1 (candidate (a)): time the wait to hand this event to its
                // consumer. Precompute (kind, peer) since `event` is moved into the
                // send; both are cheap (a match + a 32-byte copy). A send that
                // blocks past the threshold means the bounded channel stayed full
                // because the consumer (the receiver loop) was busy — direct
                // evidence a receive stalled behind a prior inline-processed send.
                let (kind, peer) = event_kind_and_peer(&event);
                let send_started = Instant::now();
                let result = tx.send(event).await;
                let wait_ms = send_started.elapsed().as_millis();
                if wait_ms > SLOW_INBOUND_DELIVERY_MS {
                    tracing::warn!(
                        kind,
                        peer = %hex32(&peer),
                        duration_ms = wait_ms as u64,
                        "inbound event delivery delayed"
                    );
                }
                match result {
                    Ok(()) => Delivery::Delivered,
                    Err(_) => Delivery::ConsumerGone,
                }
            }
            None => {
                let (kind, peer) = event_kind_and_peer(&event);
                tracing::warn!(kind, peer = %hex32(&peer), "inbound event with no consumer");
                Delivery::Orphan
            }
        }
    }

    /// Number of live ack claims (test introspection for the demux).
    #[cfg(test)]
    fn claim_count(&self) -> usize {
        self.inner
            .lock()
            .expect("demux mutex poisoned")
            .claims
            .len()
    }

    /// Route a locally-generated [`ServeProgress`](TransportEvent::ServeProgress)
    /// (Task 13) to the sender handle(s) that announced this package — matched by
    /// `package_id` across the live ack claims. A claim points at the announcing
    /// Out/Collab handle's own events channel and lives exactly for the transfer
    /// window (registered at announce, released on the ack), so a tick routes to
    /// that sender engine only while its transfer is in flight. Non-blocking
    /// (`try_send`): a full channel drops the tick, and a package with no live
    /// claim (already acked / terminal, or announced by a since-dropped handle) is
    /// silently ignored — upload progress is best-effort UI data.
    pub(crate) fn route_serve_progress(&self, package_id: &PackageId, bytes_sent: u64) {
        let inner = self.inner.lock().expect("demux mutex poisoned");
        // TODO(sync-queue follow-up): keyed on `package_id` alone, so a package
        // fanned out to N peers (N same-`package_id` claims, distinguished only by
        // `(peer, package_id)`) delivers this SAME cumulative-bytes tick to every
        // one of them — a mild cross-destination over-report (never a misroute:
        // each claim still only ever sees ITS OWN transfer's real acks). Precise
        // per-destination attribution needs the provider event's `connection_id`
        // correlated to a peer via `ClientConnectedNotify.endpoint_id` (not
        // threaded through today); out of scope for Task 13.
        for ((_, pid), (_, tx)) in inner.claims.iter() {
            if pid == package_id {
                let _ = tx.try_send(TransportEvent::ServeProgress {
                    package_id: package_id.clone(),
                    bytes_sent,
                });
            }
        }
    }

    /// Route a locally-generated [`ServeComplete`](TransportEvent::ServeComplete)
    /// (Task 2.1) to the sender handle(s) that announced this package — the
    /// terminal-success clone of [`route_serve_progress`](Self::route_serve_progress),
    /// matched by `package_id` across the live ack claims. A claim lives exactly
    /// for the transfer window (registered at announce, released on the ack), so a
    /// complete routes to that sender engine only while its transfer is in flight.
    /// Non-blocking (`try_send`): a full channel drops it, and a package with no
    /// live claim (already acked / terminal, or announced by a since-dropped
    /// handle) is silently ignored — upload-complete is best-effort UI signalling.
    /// (Same keyed-on-`package_id`-only cross-destination caveat as
    /// `route_serve_progress` above — a benign over-report to N same-id claims,
    /// never a misroute.)
    pub(crate) fn route_serve_complete(&self, package_id: &PackageId) {
        let inner = self.inner.lock().expect("demux mutex poisoned");
        for ((_, pid), (_, tx)) in inner.claims.iter() {
            if pid == package_id {
                let _ = tx.try_send(TransportEvent::ServeComplete {
                    package_id: package_id.clone(),
                });
            }
        }
    }

    /// Route a locally-generated
    /// [`ServeFileProgress`](TransportEvent::ServeFileProgress) (Task 2.2) to the
    /// sender handle(s) that announced this package — the per-file sibling of
    /// [`route_serve_progress`](Self::route_serve_progress), matched by
    /// `package_id` across the live ack claims. Non-blocking (`try_send`): a full
    /// channel drops it, and a package with no live claim is silently ignored —
    /// per-file progress is best-effort UI data.
    ///
    /// Same keyed-on-`package_id`-only cross-destination caveat as
    /// [`route_serve_progress`](Self::route_serve_progress): a package fanned out to
    /// N peers (N same-`package_id` claims) delivers this SAME per-file tick to
    /// every one of them — a benign over-report, never a misroute (each claim still
    /// only ever sees its own transfer's real acks). Precise per-destination
    /// attribution needs the provider event's `connection_id`, not threaded through
    /// today.
    pub(crate) fn route_serve_file_progress(
        &self,
        package_id: &PackageId,
        file: String,
        bytes_done: u64,
        bytes_total: u64,
    ) {
        let inner = self.inner.lock().expect("demux mutex poisoned");
        for ((_, pid), (_, tx)) in inner.claims.iter() {
            if pid == package_id {
                let _ = tx.try_send(TransportEvent::ServeFileProgress {
                    package_id: package_id.clone(),
                    file: file.clone(),
                    bytes_done,
                    bytes_total,
                });
            }
        }
    }
}

/// The inbound-event variant an orphan warn names, plus the peer it came from.
fn event_kind_and_peer(event: &TransportEvent) -> (&'static str, NodeId) {
    match event {
        TransportEvent::AnnounceReceived { from, .. } => ("announce", *from),
        TransportEvent::RevokeReceived { from, .. } => ("revoke", *from),
        TransportEvent::AckReceived { from, .. } => ("ack", *from),
        TransportEvent::ProjectAnnounceReceived { from, .. } => ("project_announce", *from),
        TransportEvent::ProjectRequestReceived { from, .. } => ("project_request", *from),
        // Locally-originated (no peer); never reach the inbound orphan path.
        TransportEvent::ServeProgress { .. } => ("serve_progress", [0u8; 32]),
        TransportEvent::ServeComplete { .. } => ("serve_complete", [0u8; 32]),
        TransportEvent::ServeFileProgress { .. } => ("serve_file_progress", [0u8; 32]),
    }
}

/// Per-peer pooled control connection (Task 2): one dialed QUIC connection per
/// peer node id, reused across control messages, idle-closed after
/// [`CONTROL_POOL_IDLE`]. Replaces the previous connect-per-message idiom so a
/// burst of announces/acks rides one connection (and spawns the path-diagnostics
/// watcher once, not per message). Any send error invalidates the entry so the
/// next send re-dials.
struct ControlPool {
    endpoint: Endpoint,
    entries: Mutex<HashMap<NodeId, PoolEntry>>,
    /// Count of real dials (test introspection: pooled reuse keeps this flat).
    dials: AtomicU64,
}

struct PoolEntry {
    conn: Connection,
    /// Updated on every reuse; the idle reaper closes the connection once it has
    /// been untouched for [`CONTROL_POOL_IDLE`].
    last_used: Arc<Mutex<Instant>>,
}

impl ControlPool {
    fn new(endpoint: Endpoint) -> Arc<Self> {
        Arc::new(Self {
            endpoint,
            entries: Mutex::new(HashMap::new()),
            dials: AtomicU64::new(0),
        })
    }

    /// A live pooled connection to `to`: reused if one is open, else freshly
    /// dialed (spawning its path-diagnostics watcher + idle reaper once).
    async fn get_or_connect(
        self: &Arc<Self>,
        to: NodeId,
        target: EndpointAddr,
    ) -> Result<Connection> {
        {
            let entries = self.entries.lock().expect("control_pool mutex poisoned");
            if let Some(entry) = entries.get(&to) {
                if entry.conn.close_reason().is_none() {
                    *entry.last_used.lock().expect("last_used mutex poisoned") = Instant::now();
                    return Ok(entry.conn.clone());
                }
            }
        }
        let conn = self
            .endpoint
            .connect(target, SYNC_ALPN)
            .await
            .context("connect sync control channel")?;
        self.dials.fetch_add(1, Ordering::Relaxed);
        // Once per pooled connection (not per message): the establishment line +
        // the relay→direct path-upgrade watcher.
        spawn_conn_path_diagnostics(&conn, "outgoing");
        let last_used = Arc::new(Mutex::new(Instant::now()));
        {
            let mut entries = self.entries.lock().expect("control_pool mutex poisoned");
            entries.insert(
                to,
                PoolEntry {
                    conn: conn.clone(),
                    last_used: Arc::clone(&last_used),
                },
            );
        }
        Arc::clone(self).spawn_idle_reaper(to, conn.clone(), last_used);
        Ok(conn)
    }

    /// Drop the pooled entry for `to` after a send error — but only if it still
    /// holds `conn` (never evict a newer re-dial), so the next send re-dials.
    fn invalidate(&self, to: NodeId, conn: &Connection) {
        let mut entries = self.entries.lock().expect("control_pool mutex poisoned");
        if entries.get(&to).map(|e| e.conn.stable_id()) == Some(conn.stable_id()) {
            entries.remove(&to);
        }
    }

    /// Close + evict a pooled connection once it has been idle for
    /// [`CONTROL_POOL_IDLE`]. One task per pooled connection; holds only a `Weak`
    /// to the pool so it never keeps it alive, and exits after it fires.
    fn spawn_idle_reaper(
        self: Arc<Self>,
        to: NodeId,
        conn: Connection,
        last_used: Arc<Mutex<Instant>>,
    ) {
        let stable_id = conn.stable_id();
        let weak = Arc::downgrade(&self);
        drop(self);
        tokio::spawn(async move {
            loop {
                let deadline =
                    *last_used.lock().expect("last_used mutex poisoned") + CONTROL_POOL_IDLE;
                let now = Instant::now();
                if now < deadline {
                    tokio::time::sleep(deadline - now).await;
                    continue;
                }
                conn.close(0u32.into(), b"idle");
                if let Some(pool) = weak.upgrade() {
                    let mut entries = pool.entries.lock().expect("control_pool mutex poisoned");
                    if entries.get(&to).map(|e| e.conn.stable_id()) == Some(stable_id) {
                        entries.remove(&to);
                    }
                }
                tracing::debug!(peer = %hex32(&to), "pooled control connection closed after idle");
                break;
            }
        });
    }
}

/// The network layer of a [`SharedIrohNode`] (Task 8, H2). The endpoint, router,
/// and control pool are built once at [`bind`](SharedIrohNode::bind) and live for
/// the node's lifetime — a relay-map change is applied IN PLACE on the live
/// endpoint ([`apply_relay_change`](SharedIrohNode::apply_relay_change),
/// `insert_relay`/`remove_relay`), so nothing here is ever swapped. Held behind
/// one lock only so the hot `endpoint`/`control_pool` accessors clone the cheap
/// Arc-backed handle out; `relay_urls`/`uses_relay` are the mutable bookkeeping a
/// hot-swap updates.
struct NetLayer {
    /// Endpoint handle (clone of the router's); dials peers + downloads blobs.
    endpoint: Endpoint,
    /// Keeps the accept loop (both protocols) alive; torn down in
    /// [`shutdown`](SharedIrohNode::shutdown). `Router` is `Clone` (Arc-backed
    /// accept task), so shutdown clones it out of the lock.
    router: Router,
    /// Per-peer pooled control connections (Task 2), reused across control sends.
    control_pool: Arc<ControlPool>,
    /// Home-relay status watcher task on this endpoint; aborted at shutdown (iroh
    /// keeps the watcher alive until the last endpoint clone drops). A relay-map
    /// hot-swap re-homes the SAME endpoint, so this one watcher observes it.
    relay_watcher: Option<JoinHandle<()>>,
    /// Transport-telemetry sampler task on this endpoint; aborted at shutdown
    /// alongside the relay watcher (same reason — it holds an endpoint clone).
    telemetry_sampler: Option<JoinHandle<()>>,
    /// The relay URLs the endpoint currently carries (H1 reporting groundwork, T7);
    /// updated in place by a relay-map hot-swap.
    relay_urls: Vec<String>,
    /// Whether a relay is configured. Gates the `online()` wait — with the relay
    /// disabled there is no home relay and `online()` would hang.
    uses_relay: bool,
}

/// The one iroh endpoint/router/store for this process (see module docs).
pub struct SharedIrohNode {
    /// The endpoint/router/control-pool/relay-config (Task 8). Behind one lock so
    /// the hot readers (`endpoint`/`control_pool` accessors) clone the cheap
    /// Arc-backed handle out and never hold the lock across an await; a relay-map
    /// change mutates the live endpoint in place rather than swapping this layer.
    net: Mutex<NetLayer>,
    /// The single blob store shared by every role handle.
    store: Store,
    /// This endpoint's node id (== ed25519 public key bytes). Stable for the node's
    /// lifetime, so peers and history keep addressing this node by the same id.
    node_id: NodeId,
    /// Known peer addresses (from pairing), used to dial the control channel.
    peers: Mutex<HashMap<NodeId, EndpointAddr>>,
    /// Endpoint address lookup — consumed by the blobs downloader when it dials
    /// by node id. Cloned handle; shares state with the endpoint.
    lookup: iroh::address_lookup::memory::MemoryLookup,
    /// Prefixed package tag (`<role>/pkg/<id>`) → collection hash, registered by
    /// [`serve`](SharingTransport::serve), injected into the wire announce by
    /// [`announce`](SharingTransport::announce). Keyed by the FULL prefixed tag
    /// so two roles serving the same package id never collide. Behind an `Arc` so
    /// the provider-upload-events consumer's [`ServeRootResolver`] (Task 13) can
    /// share it — the consumer is spawned at router build, before `Self` exists.
    served: Arc<Mutex<HashMap<String, ServedCollection>>>,
    /// Prefixed package tag (`<role>/pkg/<id>`) → the served collection's ORDERED
    /// entries `(rel_path, byte_size)` (Task 2.2), in the exact order the provider
    /// iterates the hash-seq (collection order — the SUBSET order for a want-subset
    /// serve). Keyed identically to [`served`](Self::served) and set/cleared in the
    /// same `role_serve` / `role_release` steps, so the two never drift. Behind an
    /// `Arc` for the same reason as `served`: the consumer's [`ServeFileResolver`]
    /// shares it, and is spawned at router build before `Self` exists.
    served_files: Arc<Mutex<HashMap<String, Vec<(String, u64)>>>>,
    /// Per-`(peer, package)` ack-claim + Recv-consumer router (Task 2, Д4). Owns
    /// the fan-out of the node's single inbound event stream; cloned into the
    /// control-protocol handler (as [`EventSink::Demux`]) and consulted by every
    /// role handle's [`events`](SharingTransport::events) /
    /// [`announce`](SharingTransport::announce). The single control handler holds
    /// this SAME demux for the node's lifetime, so consumers stay registered.
    demux: Arc<EventDemux>,
    /// Monotonic id source for role handles, so the demux can attribute a claim /
    /// the Recv consumer to a specific handle and release them all on its drop.
    next_handle_id: AtomicU64,
    /// Connection-level authorization gate, cloned into both protocol handlers;
    /// the host installs the predicate via [`set_connect_gate`](Self::set_connect_gate).
    connect_gate: SharedConnectGate,
    /// Dedup responder slot, cloned into the control protocol handler; the
    /// receiver installs it via [`set_dedup_responder`](Self::set_dedup_responder)
    /// when it migrates onto the node (Task 3). Empty ⇒ inbound offers are
    /// answered want-all, so nothing is ever silently withheld.
    responder: SharedResponder,
    /// Role prefixes whose startup sweep has already run (once per prefix).
    swept: Mutex<HashSet<&'static str>>,
    /// Whether the one-shot home-relay `online()` wait has run. On success it
    /// seeds `relay_health` (connected + home-relay url) so `transport_health`
    /// reports `relay_connected` immediately — a BRIDGE until the watcher records
    /// its first transition, after which the watcher is the sole authority (Task
    /// 3.3).
    online_waited: AtomicBool,
    /// Whether [`shutdown`](Self::shutdown) has already run (idempotency guard).
    shutdown_done: AtomicBool,
    /// Immediate relay-refresh trigger (H2): the sign-in path calls
    /// [`request_relay_refresh`](Self::request_relay_refresh) to wake the loop now
    /// instead of waiting for the hourly tick.
    refresh_notify: Arc<Notify>,
    /// The relay-url set the last hot-swap applied — the change-detection baseline
    /// (H2, Task 3.5). A resolver reporting a DIFFERENT set drives an in-place
    /// `insert_relay`/`remove_relay` diff; initialized to the bind-time relay set
    /// so the first unchanged resolve is a no-op.
    last_relay_urls: Mutex<Vec<String>>,
    /// The relay-refresh loop task; aborted at shutdown so no refresh runs
    /// mid-teardown. `None` until [`start_relay_refresh`](Self::start_relay_refresh)
    /// runs (once; idempotent).
    refresh_task: Mutex<Option<JoinHandle<()>>>,
    /// The held device-key advisory lock (I4); dropped at shutdown so a re-bind
    /// can re-acquire it. Held for the node's lifetime — the identity never changes.
    key_lock: Mutex<Option<DeviceKeyLock>>,
    /// Transport-level wake hook (Task 6): fired on a home-relay reconnect
    /// transition and after an applied relay-map change, so the host kicks every
    /// pending outbound package. Behind an `Arc<RwLock<…>>` so the relay watcher —
    /// spawned at bind, before the host installs the hook — reads the CURRENT hook
    /// live at fire time. Never invoked while the lock is held: the value is
    /// cloned out first, so a hook that re-enters the node can't self-deadlock.
    wake_hook: Arc<RwLock<Option<WakeHook>>>,
    /// The home-relay watcher's last-recorded transition (Task 3.3). Like
    /// [`wake_hook`](Self::wake_hook) it lives on the node's outer layer and is
    /// handed to the watcher task, which writes into this cell on every transition
    /// so [`transport_health`](Self::transport_health) always reads the current
    /// picture. Read under the lock and cloned out; never held across an await.
    relay_health: Arc<RwLock<RelayHealth>>,
    /// Transport counters as they stood at bind — the baseline the shutdown line
    /// subtracts to report the whole session's relay-vs-direct split.
    counters_at_bind: TransportCounters,
    /// Late-bindable host hook for inbound presence beacons (D1). Shared with the
    /// control protocol handler, so installing it after `bind` takes effect on the
    /// already-spawned router.
    presence: SharedPresenceHook,
    /// Device-wide upload budget consulted by the provider throttle hook (W1).
    /// Bound unlimited (rate 0) and re-rated live via
    /// [`set_upload_limit`](Self::set_upload_limit). The SAME `Arc` is held by the
    /// provider-events consumer spawned inside
    /// [`build_router`](super::build_router), so one pacer caps the whole node —
    /// every role handle, every peer, every concurrent GET — and a limit change
    /// lands on the next chunk without a rebind.
    upload_pacer: Arc<UploadPacer>,
    /// The data dir this node was bound at — blob store, and the root every
    /// host-side data path hangs off (transfer-prepare spec §4.1). Equal to the
    /// identity dir for the single-dir [`bind`](Self::bind).
    working_dir: PathBuf,
    /// How [`serve`](SharingTransport::serve) imports a package dir into the blob
    /// store; chosen by the host at bind, see [`NodeOptions`].
    serve_import_mode: ImportMode,
}

/// Host-chosen node behavior (transfer-prepare spec §4.1).
#[derive(Debug, Clone, Copy)]
pub struct NodeOptions {
    /// How `serve` imports a package dir into the blob store. `TryReference`
    /// (the app: `packages/<uuid>` is an immutable snapshot) references the
    /// payload in place and stores only the outboard; `Copy` (Perseus: a resend
    /// rewrites its payloads in place) copies it into the store.
    pub serve_import_mode: ImportMode,
}

impl Default for NodeOptions {
    fn default() -> Self {
        Self {
            serve_import_mode: ImportMode::Copy,
        }
    }
}

impl SharedIrohNode {
    /// Single-dir bind: identity and data under the same `sync_dir`, `Copy`
    /// imports — Perseus and every existing test keep this shape.
    pub async fn bind(sync_dir: &Path, relay_mode: RelayMode) -> Result<Arc<Self>> {
        Self::bind_with(sync_dir, sync_dir, relay_mode, NodeOptions::default()).await
    }

    /// The data dir this node bound at (its blob store's parent).
    pub fn working_dir(&self) -> &Path {
        &self.working_dir
    }

    /// The import mode [`serve`](SharingTransport::serve) uses for package dirs.
    pub fn serve_import_mode(&self) -> ImportMode {
        self.serve_import_mode
    }

    /// Bind the ONE endpoint for this process from the install's device key,
    /// with the IDENTITY and the DATA dirs split (transfer-prepare spec §4.1):
    /// the device key + its advisory-lock sidecar live under `identity_dir` and
    /// never move, while the blob store follows `working_dir` — so relocating
    /// the transfer working folder relocates bytes, never the identity.
    ///
    /// Takes the device-key advisory lock (fail ⇒ actionable, key-material-free
    /// error, I4/S2); builds the endpoint (`presets::Minimal` + secret +
    /// `relay_mode` + [`MemoryLookup`](iroh::address_lookup::memory::MemoryLookup));
    /// opens the single [`FsStore`] at `<working_dir>/blobs` (GC enabled once);
    /// mounts the [`Router`] with both ALPNs once; spawns the home-relay-status
    /// watcher. `relay_mode` is [`RelayMode::Default`] for production or
    /// [`RelayMode::Disabled`] for direct-only / in-process tests.
    pub async fn bind_with(
        identity_dir: &Path,
        working_dir: &Path,
        relay_mode: RelayMode,
        opts: NodeOptions,
    ) -> Result<Arc<Self>> {
        // The one device identity (task B4): the endpoint binds this secret and
        // the advisory lock is on the `device_key.lock` sidecar (NOT the key
        // file — a mandatory Windows lock there blocks account sign-in, os
        // error 33), so a second install started from a copied key fails here
        // instead of dueling on the relay (I4).
        let key = DeviceKey::load_or_create_in(identity_dir).context("load device key")?;
        let key_lock = DeviceKeyLock::acquire(&device_key_lock_path(identity_dir))?;

        let demux = EventDemux::new();
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

        // One `FsStore` at `<working_dir>/blobs` for all roles (spec §2). Mirror
        // `FsStore::load`'s internals but with GC on (load() hardcodes gc: None,
        // so released blobs would leak forever). Interval is slack (see
        // `GC_INTERVAL`) so an in-flight transfer never races collection.
        let blob_dir = working_dir.join("blobs");
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
        // Late-bindable dedup responder slot (Д3 shape): empty at bind, installed
        // by the receiver when it migrates onto the node (`set_dedup_responder`,
        // Task 3). Empty ⇒ offers are answered want-all, so nothing is ever
        // silently withheld before the receiver wires its catalog responder.
        let responder: SharedResponder = Arc::new(Mutex::new(None));
        // Presence-hook slot (D1): empty until the host installs one, so a beacon
        // arriving before the host wires up is acknowledged and dropped at debug
        // rather than queued against a consumer that does not exist yet.
        let presence: SharedPresenceHook = Arc::new(Mutex::new(None));
        // The served map lives behind an `Arc` so the provider-upload-events
        // consumer (Task 13) can resolve a served collection's root hash back to its
        // package id. Created here — before `Self` — and shared into both the
        // resolver and the struct field below.
        let served: Arc<Mutex<HashMap<String, ServedCollection>>> =
            Arc::new(Mutex::new(HashMap::new()));
        // Ordered per-package collection entries for per-file attribution (Task
        // 2.2); shared into the file resolver the same way `served` is into the
        // root resolver.
        let served_files: Arc<Mutex<HashMap<String, Vec<(String, u64)>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let serve_resolver: ServeRootResolver = {
            let served = Arc::clone(&served);
            Arc::new(move |hash| resolve_served_root_in(&served, hash))
        };
        let serve_file_resolver: ServeFileResolver = {
            let served = Arc::clone(&served);
            let served_files = Arc::clone(&served_files);
            Arc::new(move |root, index| resolve_served_file_in(&served, &served_files, root, index))
        };
        // The device-wide upload pacer (W1) is created BEFORE the router: the
        // provider-events consumer spawned inside `build_router` holds a clone and
        // consults it for every ~16 KiB payload chunk. Bound unlimited (rate 0) —
        // the host applies the persisted setting afterwards via `set_upload_limit`.
        let upload_pacer = Arc::new(UploadPacer::new(0));
        // Shared node: `EventSink::Demux` (inbound events fan out through the demux,
        // Task 2/Д4, not a single shared stream), and `flush_store_on_shutdown:
        // false` so a router teardown never tears down the store — the node flushes
        // it explicitly in `shutdown` (T8).
        let router = build_router(
            endpoint,
            &store,
            &connect_gate,
            EventSink::Demux(Arc::clone(&demux)),
            Arc::clone(&responder),
            Arc::clone(&presence),
            false,
            serve_resolver,
            serve_file_resolver,
            Arc::clone(&upload_pacer),
        );

        let endpoint = router.endpoint().clone();
        let node_id: NodeId = *endpoint.id().as_bytes();
        let control_pool = ControlPool::new(endpoint.clone());
        // The wake hook lives outside `NetLayer`; its Arc is handed to the watcher
        // task, which reads whatever hook the api layer has installed at fire time
        // (installed after bind), not a stale clone captured here at bind.
        let wake_hook: Arc<RwLock<Option<WakeHook>>> = Arc::new(RwLock::new(None));
        // The relay-health cell also lives outside `NetLayer` and is handed to the
        // watcher, which writes the node's transitions into this same cell.
        let relay_health: Arc<RwLock<RelayHealth>> = Arc::new(RwLock::new(RelayHealth::initial()));
        let relay_watcher = spawn_home_relay_watcher(
            endpoint.clone(),
            Arc::clone(&wake_hook),
            Arc::clone(&relay_health),
        );
        // Transport telemetry: the bind-time baseline for the session total, plus
        // the periodic window sampler.
        let counters_at_bind = TransportCounters::snapshot(&endpoint);
        let telemetry_sampler = spawn_transport_telemetry(endpoint.clone());

        tracing::debug!(node_id = %endpoint.id().fmt_short(), "shared iroh node bound");
        Ok(Arc::new(Self {
            net: Mutex::new(NetLayer {
                endpoint,
                router,
                control_pool,
                relay_watcher: Some(relay_watcher),
                telemetry_sampler: Some(telemetry_sampler),
                relay_urls: relay_urls.clone(),
                uses_relay,
            }),
            store,
            node_id,
            peers: Mutex::new(HashMap::new()),
            lookup,
            served,
            served_files,
            demux,
            next_handle_id: AtomicU64::new(0),
            connect_gate,
            swept: Mutex::new(HashSet::new()),
            online_waited: AtomicBool::new(false),
            shutdown_done: AtomicBool::new(false),
            refresh_notify: Arc::new(Notify::new()),
            // The change-detection baseline starts at the bind-time relay set, so
            // the first resolver run that reports the SAME set is a no-op.
            last_relay_urls: Mutex::new(relay_urls),
            refresh_task: Mutex::new(None),
            key_lock: Mutex::new(Some(key_lock)),
            responder,
            wake_hook,
            relay_health,
            counters_at_bind,
            presence,
            upload_pacer,
            working_dir: working_dir.to_path_buf(),
            serve_import_mode: opts.serve_import_mode,
        }))
    }

    /// Set this node's total sync UPLOAD limit in bytes/sec; `0` = unlimited.
    ///
    /// One budget for the whole device: every role handle, every peer and every
    /// concurrent GET draw on this pacer. Takes effect on the next ~16 KiB chunk
    /// the provider offers — including mid-transfer — because the throttle hook in
    /// the provider-events consumer holds the same `Arc`.
    pub fn set_upload_limit(&self, bytes_per_sec: u64) {
        self.upload_pacer.set_rate(bytes_per_sec);
        tracing::info!(bytes_per_sec, "sync upload limit applied");
    }

    // ----- network-layer accessors (Task 8) -----------------------------------

    /// A clone of the current endpoint handle (cheap, Arc-backed). Never holds the
    /// `net` lock across an await — callers use the returned clone.
    fn endpoint(&self) -> Endpoint {
        self.net
            .lock()
            .expect("net mutex poisoned")
            .endpoint
            .clone()
    }

    /// A clone of the current pooled-control-connection handle.
    fn control_pool(&self) -> Arc<ControlPool> {
        Arc::clone(&self.net.lock().expect("net mutex poisoned").control_pool)
    }

    /// Whether a relay is configured on the endpoint (gates the `online()` wait).
    /// A relay-map hot-swap updates this in place, so it reflects the live set.
    fn uses_relay(&self) -> bool {
        self.net.lock().expect("net mutex poisoned").uses_relay
    }

    /// This endpoint's node id (== ed25519 public key bytes).
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Resolve a served collection's root hash to the [`PackageId`] whose
    /// [`serve`](SharingTransport::serve) registered it (Task 13). The named
    /// reverse-resolution entry point + test hook; the provider-upload-events
    /// consumer resolves via a [`ServeRootResolver`] closure over the SAME shared
    /// map (it is spawned before `Self` exists, so it can't hold `&self`). `None`
    /// for a child blob / hash-seq-internal / foreign hash.
    /// Test hook (D1 Task 5): the collection hash currently recorded for a role's
    /// package tag, so a test can prove a re-serve reused it instead of importing.
    #[cfg(test)]
    pub(crate) fn resolve_served_hash_for_test(
        &self,
        role: Role,
        package_id: &PackageId,
    ) -> Option<Hash> {
        let tag = role_package_tag(role.prefix(), package_id);
        self.served
            .lock()
            .expect("served mutex poisoned")
            .get(&tag)
            .map(|e| e.hash)
    }

    #[allow(dead_code)] // exposed contract; the consumer resolves via the shared closure
    pub(crate) fn resolve_served_root(&self, hash: Hash) -> Option<PackageId> {
        resolve_served_root_in(&self.served, hash)
    }

    /// Test hook (D3 Task 1): this node's live [`TransportCounters`], so a test
    /// can bracket an operation with two snapshots and assert on the bytes it
    /// actually moved.
    ///
    /// The provider-side byte delta is the only SOUND oracle for "did this
    /// holder really serve part of the collection?". The blob downloader's
    /// per-provider progress items are best-effort and lossy on this iroh-blobs
    /// version (see [`blobs::fetch_collection_multi`]), so a swarm test that
    /// asserts on telemetry alone is asserting on a sample, not on reality.
    #[cfg(test)]
    pub(crate) fn counters_snapshot_for_test(&self) -> TransportCounters {
        TransportCounters::snapshot(&self.endpoint())
    }

    /// Live ack-claim count (test introspection for the demux).
    #[cfg(test)]
    pub(crate) fn active_claims(&self) -> usize {
        self.demux.claim_count()
    }

    /// Number of real control-connection dials (test introspection: pooled reuse
    /// keeps this flat across sends to the same peer).
    #[cfg(test)]
    pub(crate) fn control_pool_dials(&self) -> u64 {
        self.control_pool().dials.load(Ordering::Relaxed)
    }

    /// Test-only: overwrite the home-relay health cell to simulate a watcher
    /// transition (or the online-wait seed) without a live relay, so the
    /// `transport_health` derivation can be exercised deterministically (all
    /// loopback binds are relay-disabled). Task 3.3 regression coverage.
    #[cfg(test)]
    pub(crate) fn set_relay_health_for_test(&self, health: RelayHealth) {
        *self
            .relay_health
            .write()
            .expect("relay_health lock poisoned") = health;
    }

    /// Test-only: force `uses_relay` on the current net layer so the health
    /// derivation exercises the relay-configured branch without binding a real
    /// relay endpoint (which would do network I/O). Task 3.3 regression coverage.
    #[cfg(test)]
    pub(crate) fn force_uses_relay_for_test(&self, uses_relay: bool) {
        self.net.lock().expect("net mutex poisoned").uses_relay = uses_relay;
    }

    /// This endpoint's current [`EndpointAddr`] (direct addrs + relay url) — the
    /// self-reported address for H1 (Task 7). Call after a handle's
    /// [`start`](SharingTransport::start) so address discovery has settled. Reads
    /// the live endpoint, so the T7 reporter that polls this always reflects the
    /// current relay set (a hot-swap re-homes the same endpoint in place).
    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.endpoint().addr()
    }

    /// The relay URLs the endpoint currently carries (H1 groundwork, Task 7).
    pub fn relay_urls(&self) -> Vec<String> {
        self.net
            .lock()
            .expect("net mutex poisoned")
            .relay_urls
            .clone()
    }

    /// The node's current transport-reachability health for the status poll
    /// (Task 3.3), combining two signals with NO network I/O: whether a relay is
    /// configured on the current endpoint ([`uses_relay`](Self::uses_relay)) and
    /// the [`RelayHealth`] cell — written by the home-relay watcher on every
    /// transition, and seeded once by the successful one-shot `online()` wait as a
    /// bridge before the watcher's first transition. The cell is the SOLE
    /// connected-ness authority: once the watcher records a disconnect the node
    /// correctly reads `direct_only`, so a dropped relay always flips the surface
    /// (the earlier `online_ok` latch masked this — it never reset). A bound node
    /// reports only [`relay_connected`](TransportHealth::relay_connected) or
    /// [`direct_only`](TransportHealth::direct_only) — `not_started` (no node) and
    /// `no_relay_map` (signed in, no relay configuration) are api-layer
    /// derivations in [`crate::api::sync::derive_transport_health`], which owns the
    /// sign-in / cached-relay state this layer does not see.
    pub fn transport_health(&self) -> TransportHealth {
        // Relay disabled at bind (direct-only / loopback): there is no home relay,
        // so the node is undialable by peers behind NAT.
        if !self.uses_relay() {
            return TransportHealth::direct_only(None, None);
        }
        let health = self
            .relay_health
            .read()
            .expect("relay_health lock poisoned")
            .clone();
        // The cell (watcher transitions, online-wait-seeded before the first one)
        // is the sole authority. It starts disconnected, so a freshly-bound node
        // reads `direct_only` until the relay connects — and reverts to it the
        // moment the watcher records a disconnect.
        if health.connected {
            TransportHealth::relay_connected(health.url)
        } else {
            TransportHealth::direct_only(health.url, health.last_error)
        }
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
        let id = self.next_handle_id.fetch_add(1, Ordering::Relaxed);
        Arc::new(RoleHandle {
            node: Arc::clone(self),
            role,
            id,
            events_tx: Mutex::new(None),
        })
    }

    /// Install the connection-level authorization [`ConnectGate`] (governs BOTH
    /// ALPNs, S4). Overwrites any previously installed gate; left unset, the node
    /// admits every connection.
    pub fn set_connect_gate(&self, gate: ConnectGate) {
        *self
            .connect_gate
            .lock()
            .expect("connect_gate mutex poisoned") = Some(gate);
    }

    /// Install the dedup [`DedupResponder`] that answers inbound `Offer` /
    /// `FullHashes` handshakes on the control channel (Task 3 receiver migration).
    /// The running receiver installs a `CatalogDedupResponder` here so a peer's
    /// pre-Announce dedup negotiation is answered from our catalog; left unset,
    /// the node answers offers want-all (nothing silently withheld). Overwrites
    /// any previously installed responder; picked up live by the already-spawned
    /// control handler on the next inbound offer.
    pub fn set_dedup_responder(&self, responder: Arc<dyn DedupResponder>) {
        *self.responder.lock().expect("responder mutex poisoned") = Some(responder);
    }

    /// Install the host's inbound-presence callback (D1). Overwrites any previous
    /// hook and is picked up live by the already-spawned control handler; left
    /// unset, inbound beacons are acknowledged and dropped at debug.
    pub fn set_presence_hook(&self, hook: PresenceHook) {
        *self.presence.lock().expect("presence hook mutex poisoned") = Some(hook);
    }

    /// Announce to `to` that we are online and listening (D1 §3.1).
    ///
    /// Rides a DEDICATED connection, never the pooled control channel. A peer on
    /// an older build cannot decode this variant, and its accept loop breaks the
    /// whole connection on a decode failure — so the only casualty must be the
    /// beacon's own connection, leaving the pooled channel that carries announces
    /// untouched. The delivery ack is read but not required: a beacon is a hint,
    /// and the sender's retry schedule is the fallback.
    pub async fn send_presence(&self, to: NodeId) -> Result<()> {
        let target = self.dial_target(to)?;
        if target.is_empty() {
            // No addressing at all: nothing to dial, and spending the timeout on it
            // would only slow a fan-out down.
            anyhow::bail!("no known address for {}", hex32(&to));
        }
        let bytes = Msg::Presence.encode()?;
        let endpoint = self.endpoint();
        let send = async {
            let conn = endpoint
                .connect(target, SYNC_ALPN)
                .await
                .context("connect presence channel")?;
            let (mut tx, mut rx) = conn.open_bi().await.context("open presence stream")?;
            tx.write_all(&bytes).await.context("write presence")?;
            tx.finish().context("finish presence stream")?;
            let _ = rx.read_to_end(8).await;
            conn.close(0u32.into(), b"ok");
            anyhow::Ok(())
        };
        tokio::time::timeout(PRESENCE_SEND_TIMEOUT, send)
            .await
            .map_err(|_| anyhow!("presence beacon to {} timed out", hex32(&to)))??;
        Ok(())
    }

    /// Install the transport-level [`WakeHook`] (Task 6): fired on a home-relay
    /// reconnect transition and after an applied relay-map change, so the host can
    /// kick every pending outbound package the moment the node is reachable again.
    /// Overwrites any previously installed hook; picked up live by the
    /// already-spawned relay watcher (it reads the hook at fire time) and by the
    /// refresh loop. Left unset, both wake sources are silent no-ops.
    pub fn set_wake_hook(&self, hook: WakeHook) {
        *self.wake_hook.write().expect("wake_hook lock poisoned") = Some(hook);
    }

    /// Fire the installed wake hook, if any. Clones the hook out from under the
    /// read lock FIRST, then releases the lock and invokes it — so a hook that
    /// re-enters the node (or races [`set_wake_hook`](Self::set_wake_hook)) can
    /// never deadlock on the wake-hook lock. A no-op when no hook is installed.
    fn fire_wake_hook(&self) {
        let hook = self
            .wake_hook
            .read()
            .expect("wake_hook lock poisoned")
            .clone();
        if let Some(h) = hook {
            h();
        }
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

    /// Merge a relay dial hint for an inbound peer we're about to pull blobs from
    /// into the endpoint's address lookup (finding H1 / I2, T7). Built from OUR
    /// own relay set — same-account peers ride the same hub relays — so an inbound
    /// peer with no cached direct path still gets a relay route for the blob pull.
    /// Written ONLY into the address lookup the blobs downloader dials through,
    /// never the control-dial `peers` map, and as a MERGE (`add_endpoint_info`),
    /// so it can never downgrade a richer address the node already knows. An empty
    /// relay set yields a bare addr and is a no-op — exactly the pre-T7 behavior.
    pub fn add_peer_dial_hint(&self, from: NodeId) {
        let relay_urls = self.relay_urls();
        match crate::sync::pairing::peer_addr_with_relays(from, &relay_urls) {
            Ok(addr) if !addr.is_empty() => self.lookup.add_endpoint_info(addr),
            Ok(_) => {} // no relays resolved → nothing to hint (current behavior)
            Err(e) => tracing::warn!(
                error = %format!("{e:#}"),
                peer = %hex32(&from),
                "add_peer_dial_hint: address build failed"
            ),
        }
    }

    /// Remove a peer's cached address-lookup entry (iroh hardening T8 / Task 3.6).
    ///
    /// Called by the sync engine's retry path just before it re-registers a
    /// freshly re-resolved address for a peer whose last dial failed with a
    /// stale-addressing class. A [`MemoryLookup`](iroh::address_lookup::memory::MemoryLookup)
    /// merge (`add_endpoint_info`) accumulates addresses and can never drop a
    /// now-dead relay URL, so removing the entry first turns the subsequent
    /// [`add_peer`](Self::add_peer) into a full REPLACE instead of a merge. Only
    /// the address lookup is cleared — the control-dial `peers` book is overwritten
    /// (not accumulated) by `add_peer`, so re-adding the fresh address restores it
    /// correctly. A no-op when the id is unparseable or absent from the lookup.
    pub fn remove_peer_addr(&self, peer: NodeId) {
        match EndpointId::from_bytes(&peer) {
            Ok(id) => {
                self.lookup.remove_endpoint_info(id);
            }
            Err(e) => tracing::warn!(
                peer = %hex32(&peer),
                error = %e,
                "remove_peer_addr: invalid node id"
            ),
        }
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

    /// Gracefully tear down the node (I1): abort the relay refresh loop + relay
    /// watcher, then `Router::shutdown().await` → store shutdown → bounded
    /// `endpoint.close()`, then release the device-key lock. Idempotent — a second
    /// call is a no-op.
    pub async fn shutdown(&self) {
        if self.shutdown_done.swap(true, Ordering::SeqCst) {
            return;
        }
        // Abort the relay-refresh loop first (Task 8): setting `shutdown_done`
        // above means a refresh iteration racing this abort re-checks the flag
        // after its resolver await and bails before touching the endpoint.
        if let Some(handle) = self
            .refresh_task
            .lock()
            .expect("refresh_task mutex poisoned")
            .take()
        {
            handle.abort();
        }
        // Take the router + endpoint + relay watcher out of the network layer.
        // `Router` is `Clone` (shared accept task), so cloning it out of the lock
        // and shutting the clone down tears the one accept task down.
        let (router, endpoint, watcher, sampler) = {
            let mut net = self.net.lock().expect("net mutex poisoned");
            (
                net.router.clone(),
                net.endpoint.clone(),
                net.relay_watcher.take(),
                net.telemetry_sampler.take(),
            )
        };
        // Abort the relay watcher: iroh keeps the `home_relay_status` watcher alive
        // until the last endpoint clone drops (closing the endpoint does NOT stop
        // it), so it must be aborted explicitly. Same for the telemetry sampler.
        if let Some(handle) = watcher {
            handle.abort();
        }
        if let Some(handle) = sampler {
            handle.abort();
        }
        // Session total BEFORE the endpoint closes — the one line that answers
        // "how much of this run's traffic rode a relay?" without log arithmetic.
        TransportCounters::snapshot(&endpoint)
            .delta(&self.counters_at_bind)
            .log("session");
        if let Err(e) = router.shutdown().await {
            tracing::warn!(error = %e, "shared iroh node router shutdown");
        }
        // Flush the persistent store to disk (also releases the redb file lock
        // so a later re-bind over the same dir can reopen it).
        if let Err(e) = self.store.shutdown().await {
            tracing::warn!(error = %e, "shared iroh node blob store shutdown");
        }
        if tokio::time::timeout(SHUTDOWN_CLOSE_TIMEOUT, endpoint.close())
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
        drop(
            self.key_lock
                .lock()
                .expect("key_lock mutex poisoned")
                .take(),
        );
        tracing::info!(node_id = %hex32(&self.node_id), "shared iroh node shut down");
    }

    // ----- relay-map lifecycle: refresh loop + live hot-swap (Task 8, H2/3.5) --

    /// Start the hourly relay-map refresh loop (H2), bounding relay-map staleness.
    ///
    /// The loop re-runs the injected, hub-agnostic `resolver` on an hourly tick (or
    /// immediately when [`request_relay_refresh`](Self::request_relay_refresh) is
    /// called on the sign-in path). On a CHANGED relay-url set it applies the diff
    /// to the LIVE endpoint via `insert_relay`/`remove_relay`
    /// ([`apply_relay_change`](Self::apply_relay_change)) — a connection-preserving
    /// re-home, no endpoint/router rebuild. Idempotent: a second call is a no-op
    /// (the first resolver wins), so the app's several start entry points can all
    /// call it.
    pub fn start_relay_refresh(self: &Arc<Self>, resolver: RelayResolver) {
        let mut guard = self
            .refresh_task
            .lock()
            .expect("refresh_task mutex poisoned");
        if guard.is_some() {
            return;
        }
        let weak = Arc::downgrade(self);
        let notify = Arc::clone(&self.refresh_notify);
        let handle = tokio::spawn(async move {
            loop {
                // Re-run the resolver on the hourly tick or an immediate trigger.
                tokio::select! {
                    _ = tokio::time::sleep(RELAY_REFRESH_INTERVAL) => {}
                    _ = notify.notified() => {}
                }
                let Some(node) = weak.upgrade() else { return };
                if node.shutdown_done.load(Ordering::SeqCst) {
                    return;
                }
                match resolver().await {
                    Some((mode, _urls)) => {
                        // A relay-map change re-homes the live endpoint onto a
                        // fresh relay set — a wake event (Task 6): kick every
                        // pending outbound package so it re-announces on the new
                        // map instead of waiting out its backoff.
                        if node.apply_relay_change(mode).await {
                            node.fire_wake_hook();
                        }
                    }
                    None => {
                        // The resolver couldn't resolve the current map (hub blip):
                        // the endpoint keeps the relay set it already carries, but
                        // surface it — a silent keep would hide a hub that has gone
                        // unreachable for hours.
                        tracing::warn!("relay map refresh failed; keeping current map");
                    }
                }
            }
        });
        *guard = Some(handle);
    }

    /// Trigger an immediate relay-map refresh (the sign-in path): wake the refresh
    /// loop now instead of waiting for the hourly tick. A no-op if the loop is not
    /// running yet — the eventual [`start_relay_refresh`](Self::start_relay_refresh)
    /// will resolve at once on its first iteration.
    pub fn request_relay_refresh(&self) {
        self.refresh_notify.notify_one();
    }

    /// Apply a fresh resolver result to the LIVE endpoint (Task 3.5): diff the new
    /// relay set against the last-applied one and re-home in place via
    /// [`Endpoint::insert_relay`]/[`Endpoint::remove_relay`] — no endpoint/router
    /// rebuild, so in-flight QUIC connections and the pooled control channel are
    /// preserved (iroh re-STUNs and re-homes the SAME endpoint id). Compares as
    /// sorted sets so relay-url ordering is not a spurious change. Returns `true`
    /// iff the set actually changed (a diff was applied) — the refresh loop fires
    /// the wake hook on that signal (Task 6).
    async fn apply_relay_change(&self, mode: RelayMode) -> bool {
        // The authoritative new set — canonical `RelayUrl`s plus their configs,
        // derived from the resolved mode exactly as `bind` seeds the baseline.
        let new_map = mode.relay_map();
        let new_urls: Vec<RelayUrl> = new_map.urls::<Vec<_>>();
        let new_strs: Vec<String> = new_urls.iter().map(|u| u.to_string()).collect();

        let old_strs: Vec<String> = self
            .last_relay_urls
            .lock()
            .expect("last_relay_urls mutex poisoned")
            .clone();
        let (added, removed) = relay_set_diff(&old_strs, &new_strs);
        if added.is_empty() && removed.is_empty() {
            return false; // unchanged — nothing to hot-swap
        }

        let endpoint = self.endpoint();
        // Insert each newly-present relay with its freshly-resolved config.
        for url in &new_urls {
            if added.contains(&url.to_string()) {
                if let Some(config) = new_map.get(url) {
                    endpoint.insert_relay(url.clone(), config).await;
                }
            }
        }
        // Remove each relay no longer in the set.
        for s in &removed {
            match s.parse::<RelayUrl>() {
                Ok(url) => {
                    endpoint.remove_relay(&url).await;
                }
                Err(e) => tracing::warn!(
                    url = %s,
                    error = %e,
                    "relay hot-swap: unparseable removed relay url; skipping removal"
                ),
            }
        }

        // Update the bookkeeping the T7 reporter (`relay_urls`) and the `online()`
        // gate (`uses_relay`) read, plus the change-detection baseline.
        let uses_relay = !matches!(mode, RelayMode::Disabled);
        {
            let mut net = self.net.lock().expect("net mutex poisoned");
            net.relay_urls = new_strs.clone();
            net.uses_relay = uses_relay;
        }
        *self
            .last_relay_urls
            .lock()
            .expect("last_relay_urls mutex poisoned") = new_strs;
        tracing::info!(
            added = added.len(),
            removed = removed.len(),
            relay_count = new_urls.len(),
            "relay map hot-swapped on live endpoint"
        );
        true
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

    /// Short control-level reachability probe of `to` (Task 9), run by the download
    /// orchestrator before committing to a holder's long blob poll: a cheap
    /// `Endpoint::connect` on the control ALPN, bounded by `timeout`, classified
    /// into a [`ProbeClass`] on failure.
    ///
    /// `Ok(())` ⇒ the control connection established and stayed open past
    /// [`PROBE_CLOSE_WINDOW`] (reachable). `Err(class)`:
    /// - **Refused** — the connection established but the peer's [`ConnectGate`]
    ///   closed it (authz refusal), or `connect` errored with an application close.
    /// - **RelayUnreachable** — the dial timed out with a relay hint present.
    /// - **Offline** — no addressing info / no route (or a dial timeout with no
    ///   relay hint).
    ///
    /// A fresh connection is used (not the pooled one) so a refused/closing
    /// connection never pollutes the control pool; it is dropped (closed) on return,
    /// and the subsequent `request_project` dials its own pooled connection. This is
    /// a best-effort diagnostic — a reported class can misdirect a retry but never
    /// authenticate a holder (S5); content-hash + peer-authz remain the trust
    /// boundary.
    pub async fn probe_holder(
        &self,
        to: NodeId,
        has_relay_hint: bool,
        timeout: Duration,
    ) -> std::result::Result<(), ProbeClass> {
        let target = self.dial_target(to).map_err(|_| ProbeClass::Offline)?;
        // A target with neither a direct addr nor a relay is undialable — offline
        // without spending the timeout (deterministic; the download path always
        // attaches a relay dial hint first, so this only short-circuits a genuinely
        // address-less peer).
        if target.is_empty() {
            return Err(ProbeClass::Offline);
        }
        let endpoint = self.endpoint();
        match tokio::time::timeout(timeout, endpoint.connect(target, SYNC_ALPN)).await {
            Ok(Ok(conn)) => {
                // Handshake + ALPN succeeded. A refusing gate closes the fresh
                // connection almost immediately; a bounded wait separates refused
                // (peer closed) from reachable (stays open). The connection is
                // dropped (closed) at scope end either way.
                match tokio::time::timeout(PROBE_CLOSE_WINDOW, conn.closed()).await {
                    Ok(_reason) => Err(ProbeClass::Refused),
                    Err(_) => Ok(()),
                }
            }
            Ok(Err(e)) => Err(classify_connect_err(&format!("{e}"), has_relay_hint)),
            Err(_) => Err(if has_relay_hint {
                ProbeClass::RelayUnreachable
            } else {
                ProbeClass::Offline
            }),
        }
    }

    /// Open a bidi stream on the peer's pooled control connection, send one
    /// [`Msg`], and wait for the peer's application-level delivery ack. The
    /// connection is NOT closed after the send — it stays pooled for reuse (Task
    /// 2); the ack-before-return semantics are unchanged verbatim. Any error
    /// invalidates the pooled entry so the next send re-dials.
    async fn send_control(&self, to: NodeId, msg: Msg) -> Result<()> {
        let target = self.dial_target(to)?;
        let bytes = msg.encode()?;
        // Clone the pooled-control handle out of the `net` lock so it is not held
        // across the awaits below.
        let pool = self.control_pool();
        let conn = pool.get_or_connect(to, target).await?;

        let send = async {
            let (mut tx, mut rx) = conn.open_bi().await.context("open control stream")?;
            tx.write_all(&bytes)
                .await
                .context("write control message")?;
            tx.finish().context("finish control stream")?;
            let ack = rx
                .read_to_end(8)
                .await
                .context("await control delivery ack")?;
            if ack.is_empty() {
                anyhow::bail!("control message not acknowledged by peer");
            }
            anyhow::Ok(())
        };

        match tokio::time::timeout(CONTROL_SEND_TIMEOUT, send).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                pool.invalidate(to, &conn);
                Err(e)
            }
            Err(_) => {
                pool.invalidate(to, &conn);
                Err(anyhow!("sync control send to {} timed out", hex32(&to)))
            }
        }
    }

    /// Open a control connection to `to`, send one request [`Msg`], and read the
    /// peer's **reply `Msg`** off the same bidi stream (the request/response
    /// counterpart of [`send_control`](Self::send_control)). Used by the dedup
    /// handshake; each round drives its own connection/stream.
    async fn send_request(&self, to: NodeId, msg: Msg) -> Result<Msg> {
        let target = self.dial_target(to)?;
        let bytes = msg.encode()?;
        let pool = self.control_pool();
        let conn = pool.get_or_connect(to, target).await?;

        let exchange = async {
            let (mut tx, mut rx) = conn.open_bi().await.context("open control stream")?;
            tx.write_all(&bytes)
                .await
                .context("write control request")?;
            tx.finish().context("finish control request")?;
            let reply = rx
                .read_to_end(MAX_CONTROL_BYTES)
                .await
                .context("await control reply")?;
            if reply.is_empty() {
                anyhow::bail!("control request closed without a reply");
            }
            let reply = Msg::decode(&reply).context("decode control reply")?;
            anyhow::Ok(reply)
        };

        match tokio::time::timeout(CONTROL_SEND_TIMEOUT, exchange).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(e)) => {
                pool.invalidate(to, &conn);
                Err(e)
            }
            Err(_) => {
                pool.invalidate(to, &conn);
                Err(anyhow!("sync control request to {} timed out", hex32(&to)))
            }
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
        if self
            .swept
            .lock()
            .expect("swept mutex poisoned")
            .insert(prefix)
        {
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
        let endpoint = self.endpoint();
        if self.uses_relay() && !self.online_waited.swap(true, Ordering::SeqCst) {
            match tokio::time::timeout(ONLINE_TIMEOUT, endpoint.online()).await {
                Ok(()) => {
                    let relay_url = endpoint.addr().relay_urls().next().map(|u| u.to_string());
                    // Seed the health cell for the status surface (Task 3.3): the
                    // relay is connected right now, so `transport_health` can report
                    // `relay_connected` immediately, before the watcher records its
                    // first transition. This is a BRIDGE only — the watcher then
                    // overwrites this cell on every later transition (including a
                    // disconnect), so it never masks a dropped relay.
                    *self
                        .relay_health
                        .write()
                        .expect("relay_health lock poisoned") = RelayHealth {
                        connected: true,
                        url: relay_url.clone(),
                        last_error: None,
                        since: Instant::now(),
                    };
                    tracing::info!(
                        node_id = %endpoint.id().fmt_short(),
                        relay_url = relay_url.as_deref().unwrap_or("unknown"),
                        "home relay connected"
                    );
                }
                Err(_) => {
                    // The wait ran and timed out: leave the health cell to the
                    // watcher (its initial baseline is disconnected), so we are
                    // direct-only until the watcher sees a connection.
                    tracing::warn!(
                        node_id = %endpoint.id().fmt_short(),
                        timeout_ms = ONLINE_TIMEOUT.as_millis() as u64,
                        "home relay wait timed out; proceeding on direct addresses only (unreachable behind NAT)"
                    );
                }
            }
        }

        let addr = endpoint.addr();
        let pairing_ticket = EndpointTicket::from(addr).to_string();
        tracing::debug!(role = prefix, node_id = %endpoint.id().fmt_short(), "shared iroh node role started");
        Ok(StartInfo {
            node_id: self.node_id,
            pairing_ticket,
        })
    }

    async fn role_announce(
        &self,
        role: Role,
        to: NodeId,
        a: &PackageAnnounce,
        batch_name: &str,
        batch_uuid: &str,
        files: &[AnnounceFileEntry],
        layout: PackageLayout,
    ) -> Result<()> {
        let tag = role_package_tag(role.prefix(), &a.package_id);
        let mut wire = a.clone();
        {
            let served = self.served.lock().expect("served mutex poisoned");
            match served.get(&tag) {
                Some(entry) => wire.root_hash = entry.hash.to_string(),
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

    async fn revoke_inner(
        &self,
        to: NodeId,
        package_id: &PackageId,
        reason: RevokeReason,
    ) -> Result<()> {
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
                Some(entry) => wire.root_hash = entry.hash.to_string(),
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
        let tag = role_package_tag(role.prefix(), &pkg.package_id);
        // Clone the endpoint out of the `net` lock so it is not held across the
        // download awaits below.
        let endpoint = self.endpoint();
        blobs::fetch_collection_to_dir(
            &self.store,
            &endpoint,
            provider,
            root_hash,
            &tag,
            dest_dir,
            pkg.byte_size,
            sink,
        )
        .await?;

        // Promoted from debug to info WITH the path the payload actually took:
        // this fires once per package (operation lifecycle, not a hot path), and
        // it is the only place the app can state whether a transfer went direct
        // or rode the relay — the bulk of it runs inside iroh-blobs' own
        // connection pool, which never hands us a `Connection` to instrument.
        let conn_path = peer_conn_path(&endpoint, from).await;
        tracing::info!(
            from = %hex32(&from),
            package_id = %pkg.package_id.0,
            conn_path,
            "iroh fetch complete"
        );
        Ok(())
    }

    /// The swarm counterpart of [`role_fetch`](Self::role_fetch) (D3 Task 1):
    /// one collection, the whole holder set, split per child blob.
    ///
    /// **Tag derivation.** `role_fetch` names its tag after the announce's
    /// package id; a swarm fetch has no announce — the hub's `root_hash` IS the
    /// subject — so the tag is the same `<role>/pkg/<id>` shape keyed by the
    /// collection hash's canonical lowercase hex: `<role>/pkg/<root_hash>`. A hex
    /// hash is a single path segment, exactly like a package uuid, so every
    /// existing namespace rule keeps working unchanged: the role-prefixed startup
    /// sweep still scopes the permanent tag, and
    /// [`role_release`](Self::role_release) reclaims BOTH it and its
    /// `in-flight/` derivative when called with `PackageId(root_hash_hex)`. It is
    /// also idempotent under content addressing: two members fetching the same
    /// collection reuse one tag name, and a re-fetch overwrites rather than
    /// leaking a second.
    ///
    /// **B7 sweep interaction (role-dependent).** The swarm fetch's caller is the
    /// collab exchange, i.e. [`Role::Collab`], so its in-flight tag is
    /// `in-flight/collab/pkg/<root_hash>` — outside the `in-flight/recv/pkg/`
    /// prefix the receiver's orphan sweep enumerates, so the contract holds by
    /// exclusion. Were a `Role::Recv` caller ever added, its in-flight tag WOULD
    /// fall inside that prefix and the sweep would reclaim it as an orphan (no
    /// `sync_inbound` row is ever keyed by a collection hash). That costs a
    /// resume, never integrity — but it is the reason to keep swarm fetches on
    /// the Collab handle.
    async fn role_fetch_multi(
        &self,
        role: Role,
        providers: Vec<NodeId>,
        root_hash: &str,
        byte_size: u64,
        dest_dir: &Path,
        sink: FetchSink,
        telemetry: ProviderTelemetrySink,
    ) -> Result<()> {
        let hash: Hash = root_hash
            .parse()
            .with_context(|| format!("parse collection hash {root_hash:?}"))?;
        if providers.is_empty() {
            anyhow::bail!("multi-source fetch of {hash} has no providers");
        }
        let ids = providers
            .iter()
            .map(|p| {
                EndpointId::from_bytes(p).map_err(|e| anyhow!("invalid provider node id: {e}"))
            })
            .collect::<Result<Vec<EndpointId>>>()?;
        let tag = role_package_tag(role.prefix(), &PackageId(hash.to_hex()));
        // Clone the endpoint out of the `net` lock so it is not held across the
        // download awaits below (same discipline as `role_fetch`).
        let endpoint = self.endpoint();
        let count = ids.len();
        blobs::fetch_collection_multi(
            &self.store,
            &endpoint,
            ids,
            hash,
            &tag,
            dest_dir,
            byte_size,
            sink,
            telemetry,
        )
        .await?;

        tracing::info!(
            providers = count,
            root_hash = %hash,
            "iroh multi-source fetch complete"
        );
        Ok(())
    }

    async fn role_fetch_manifest(
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
        // Clone the endpoint out of the `net` lock so it is not held across the await.
        let endpoint = self.endpoint();
        blobs::fetch_manifest_to_dir(&self.store, &endpoint, provider, root_hash, dest_dir).await
    }

    /// [`SharingTransport::protect_shared_before_cleanup`] for a role handle: make
    /// this package's shared blobs outlive the payload deletion the engine is
    /// about to perform (spec §4.2). Runs BEFORE the cleanup, so
    /// `entry.src_dir` — the package's own payload dir — is still on disk.
    async fn role_protect_shared_before_cleanup(
        &self,
        role: Role,
        package_id: &PackageId,
    ) -> Result<()> {
        let tag = role_package_tag(role.prefix(), package_id);
        let entry = self
            .served
            .lock()
            .expect("served mutex poisoned")
            .get(&tag)
            .cloned();
        let Some(entry) = entry else {
            // Nothing served under this tag in THIS process (a restart, or a
            // transport that never served it) — there is nothing to protect.
            return Ok(());
        };
        let sizes = self
            .served_files
            .lock()
            .expect("served_files mutex poisoned")
            .get(&tag)
            .cloned()
            .unwrap_or_default();
        self.copy_shared_children(&tag, package_id, &entry, &sizes)
            .await
    }

    /// Copy into the store every child of `entry` that ANOTHER live collection
    /// also references, so those blobs stop depending on any payload dir.
    ///
    /// Under [`ImportMode::TryReference`] a served blob is a REFERENCE to a file
    /// under `packages/<uuid>`, and iroh keeps a sorted UNION of external paths
    /// per hash, reading `paths.first()`. Two packages carrying the same bytes
    /// therefore share one entry whose first path may belong to whichever of them
    /// finishes first — and that one's dir is about to be deleted, while the
    /// survivor's tag keeps the entry alive past GC so nothing would ever heal it.
    /// Re-importing the shared children with [`ImportMode::Copy`] makes them
    /// `Owned`, which wins the union from both sides.
    ///
    /// Sources are tried in order: this package's own file (the caller runs before
    /// the cleanup, so it is there), then the sharing package's file — the
    /// fallback that keeps this correct if our dir is already gone (a crash
    /// between cleanup and a resumed release, an out-of-band purge).
    ///
    /// Scope guards: `Copy` hosts (Perseus) own their blobs already and return
    /// immediately; children at or below the inline threshold are never external;
    /// a collection that no longer loads is skipped with a `debug!` (nothing left
    /// to protect). A child that is shared but has no readable source anywhere is
    /// an `error!` + `Err` — the caller then keeps the payload on disk rather than
    /// deleting bytes another package still needs.
    async fn copy_shared_children(
        &self,
        tag: &str,
        package_id: &PackageId,
        entry: &ServedCollection,
        sizes: &[(String, u64)],
    ) -> Result<()> {
        use iroh_blobs::api::blobs::AddPathOptions;
        use iroh_blobs::format::collection::Collection;
        use iroh_blobs::BlobFormat;
        use n0_future::StreamExt as _;

        if !matches!(self.serve_import_mode, ImportMode::TryReference) {
            return Ok(());
        }
        let mine = match Collection::load(entry.hash, &self.store).await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    package_id = %package_id.0,
                    root_hash = %entry.hash,
                    error = %e,
                    "released collection no longer loads; nothing to protect"
                );
                return Ok(());
            }
        };
        let size_by_name: HashMap<&str, u64> =
            sizes.iter().map(|(n, s)| (n.as_str(), *s)).collect();

        // Every OTHER live hash-seq tag is a collection that may reference our
        // children. Listing only `HashSeq` tags is the same namespace contract the
        // in-flight/seed tags already rely on; a tag whose collection cannot be
        // loaded (a partial receive, a half-swept root) simply contributes nothing.
        let mut other_tags: Vec<(String, Hash)> = Vec::new();
        let mut stream = self
            .store
            .tags()
            .list_hash_seq()
            .await
            .map_err(|e| anyhow!("list hash-seq tags: {e}"))?;
        while let Some(info) = stream.next().await {
            let info = info.map_err(|e| anyhow!("list hash-seq tag entry: {e}"))?;
            let name = String::from_utf8_lossy(info.name.as_ref()).into_owned();
            if name != tag {
                other_tags.push((name, info.hash));
            }
        }

        let mut shared: HashSet<Hash> = HashSet::new();
        // A live file for a shared child that is NOT under our own dir: the sharing
        // package's own payload, which by definition still exists (its tag is live).
        let mut alt_source: HashMap<Hash, PathBuf> = HashMap::new();
        for (other_tag, other_hash) in other_tags {
            let coll = match Collection::load(other_hash, &self.store).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!(
                        tag = %other_tag,
                        root_hash = %other_hash,
                        error = %e,
                        "live tag's collection does not load; skipped when scanning for shared blobs"
                    );
                    continue;
                }
            };
            let other_dir = self
                .served
                .lock()
                .expect("served mutex poisoned")
                .get(&other_tag)
                .map(|e| e.src_dir.clone());
            for (name, child) in coll.iter() {
                shared.insert(*child);
                if let Some(dir) = &other_dir {
                    alt_source.entry(*child).or_insert_with(|| dir.join(name));
                }
            }
        }

        let mut copied = 0usize;
        for (name, child) in mine.iter() {
            if !shared.contains(child) {
                continue;
            }
            // Unknown size ⇒ treat as external and protect it (conservative).
            if size_by_name.get(name.as_str()).copied().unwrap_or(u64::MAX)
                <= blobs::INLINE_BLOB_MAX_BYTES
            {
                continue;
            }
            // An already-owned child would be copied again here — there is no public
            // API to ask where a blob lives, and a successful read proves nothing
            // (it may be reading OUR file, the one about to be deleted). The waste is
            // bounded to blobs two live packages genuinely share.
            let own = entry.src_dir.join(name);
            let src = if own.is_file() {
                own
            } else {
                match alt_source.get(child) {
                    Some(p) if p.is_file() => p.clone(),
                    _ => {
                        tracing::error!(
                            package_id = %package_id.0,
                            hash = %child,
                            path = %own.display(),
                            "shared blob has no readable source; refusing to release it"
                        );
                        anyhow::bail!(
                            "no readable source for shared blob {child} ({})",
                            own.display()
                        );
                    }
                }
            };
            let tt = self
                .store
                .blobs()
                .add_path_with_opts(AddPathOptions {
                    path: src.clone(),
                    format: BlobFormat::Raw,
                    mode: ImportMode::Copy,
                })
                .temp_tag()
                .await
                .with_context(|| format!("copy shared blob {}", src.display()))?;
            if tt.hash() != *child {
                tracing::error!(
                    package_id = %package_id.0,
                    hash = %child,
                    copied_hash = %tt.hash(),
                    path = %src.display(),
                    "shared blob source changed on disk; refusing to release it"
                );
                anyhow::bail!(
                    "shared blob source {} changed on disk ({} → {})",
                    src.display(),
                    child,
                    tt.hash()
                );
            }
            // Dropping the temp tag is safe: the sharing package's tag pins the hash,
            // and the bytes are store-owned now.
            copied += 1;
        }
        if copied > 0 {
            tracing::info!(
                package_id = %package_id.0,
                count = copied,
                "shared blobs copied into the store before release"
            );
        }
        Ok(())
    }

    /// Is every child of an already-imported collection still readable? The
    /// re-serve short-circuit's guard.
    ///
    /// Reusing a `served` entry skips the import — and with it
    /// [`blobs::ensure_child_readable`], the thing that repairs a dead external
    /// path. A package whose sibling was released and cleaned in the meantime can
    /// therefore be re-announced pointing at deleted files. One byte per
    /// above-inline child answers it; `false` sends the caller through a fresh
    /// import, which repairs. `Copy` hosts never have external children, so they
    /// short-circuit to `true` without reading anything.
    async fn served_collection_is_readable(&self, tag: &str, hash: Hash) -> bool {
        use iroh_blobs::format::collection::Collection;

        if !matches!(self.serve_import_mode, ImportMode::TryReference) {
            return true;
        }
        let sizes: HashMap<String, u64> = self
            .served_files
            .lock()
            .expect("served_files mutex poisoned")
            .get(tag)
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default();
        let coll = match Collection::load(hash, &self.store).await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    tag,
                    root_hash = %hash,
                    error = %e,
                    "served collection no longer loads; re-importing"
                );
                return false;
            }
        };
        for (name, child) in coll.iter() {
            if sizes.get(name).copied().unwrap_or(u64::MAX) <= blobs::INLINE_BLOB_MAX_BYTES {
                continue;
            }
            if let Err(e) = blobs::probe_first_byte(&self.store, *child).await {
                tracing::debug!(
                    tag,
                    hash = %child,
                    error = %format!("{e:#}"),
                    "served child is unreadable"
                );
                return false;
            }
        }
        true
    }

    async fn role_serve(
        &self,
        role: Role,
        pkg: &PackageAnnounce,
        src_dir: &Path,
        want: Option<&HashSet<String>>,
    ) -> Result<()> {
        let tag = role_package_tag(role.prefix(), &pkg.package_id);
        // The import returns the collection hash PLUS the ordered `(rel_path, size)`
        // entries in provider-iteration order — recorded in `served_files` for
        // per-file upload attribution by hash-seq index (Task 2.2).
        // Audit F8: every retry calls `serve` first, and importing re-hashes the
        // whole package directory — a multi-GB re-read paid before the announce
        // even discovers the peer is absent. Reuse what THIS process already
        // imported for the same (tag, want-subset); `role_release` clears the entry,
        // so a rebuilt payload (a Perseus resend) still imports for real, and after
        // a restart the map is empty so the first serve imports again — correct,
        // because the blob store's tags may have been swept meanwhile.
        let fingerprint = want_fingerprint(want);
        let reusable = {
            let served = self.served.lock().expect("served mutex poisoned");
            served
                .get(&tag)
                .filter(|entry| entry.want_fingerprint == fingerprint)
                .map(|entry| entry.hash)
        };
        if let Some(hash) = reusable {
            // The short-circuit skips the import, and with it the repair
            // `ensure_child_readable` performs — so a collection whose referenced
            // files went away since (a sibling package released + cleaned) would be
            // re-announced dead. Probe before trusting it.
            if self.served_collection_is_readable(&tag, hash).await {
                tracing::debug!(
                    package_id = %pkg.package_id.0,
                    root_hash = %hash,
                    "serve reuses the already-imported collection"
                );
                return Ok(());
            }
            tracing::warn!(
                package_id = %pkg.package_id.0,
                root_hash = %hash,
                "served collection reads a dead path; re-importing"
            );
            self.forget_served_tag(&tag);
        }
        // The host's import mode (spec §4.1): the app references its immutable
        // `packages/<uuid>` in place, Perseus copies (its resend rewrites the same
        // dir). Both import paths honor it — a dedup-narrowed subset send must not
        // fall back to a copy.
        let mode = self.serve_import_mode;
        let (hash, entries) = match want {
            None => {
                blobs::import_package_collection_with_mode(&self.store, src_dir, &tag, mode).await?
            }
            Some(w) => blobs::import_subset_collection(&self.store, src_dir, w, &tag, mode).await?,
        };
        self.served.lock().expect("served mutex poisoned").insert(
            tag.clone(),
            ServedCollection {
                hash,
                want_fingerprint: fingerprint,
                src_dir: src_dir.to_path_buf(),
            },
        );
        self.served_files
            .lock()
            .expect("served_files mutex poisoned")
            .insert(tag, entries);
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
        self.served
            .lock()
            .expect("served mutex poisoned")
            .remove(&tag);
        // Drop the per-file attribution entries in lock-step with `served` (Task
        // 2.2) so the two maps never drift.
        self.served_files
            .lock()
            .expect("served_files mutex poisoned")
            .remove(&tag);
        let removed = self
            .store
            .tags()
            .delete(tag.as_bytes())
            .await
            .map_err(|e| anyhow!("delete package tag: {e}"))?;
        // Task 2.3 orphan hygiene: a terminal receiver outcome (Done via ack, or
        // Cancelled via the epilogue) routes through here, so this is the seam that
        // reclaims an in-flight download tag whose fetch errored or was cancelled
        // without ever completing (the fetch itself deletes it only on success).
        // Best-effort: a stray delete (there is none on the serve/sender side) must
        // never fail a release. Never swallow: log first.
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

    /// **Seed a project package** (D3 §3.4): import `pkg_dir` as a collection
    /// pinned under the reserved `project/<project_id>/<package_id>` tag
    /// (`PROJECT_SEED_PREFIX`), and return the collection [`Hash`].
    ///
    /// That hash is the package's REAL `root_hash`: the publisher sends it to the
    /// hub at announce (D3 T2) and every downloader fetches + BLAKE3-verifies
    /// against it, from any holder, in any mix — which is what makes the
    /// announcement swarm-capable. Seeding is puller-driven from here on: the
    /// blobs simply being resident under a permanent tag is what lets an incoming
    /// GET be served, with no control-message round trip.
    ///
    /// Imported with [`ImportMode::TryReference`], so the blobs REFERENCE the
    /// files under `pkg_dir` in place — seeding costs metadata, not a second copy
    /// of every frame. `pkg_dir` must therefore stay put and stay unchanged for as
    /// long as the tag lives (publisher: the retained `collab_pub/<package_id>`
    /// dir; downloader: the landed contribution files). A rewritten file makes
    /// this seed fail verification at every downloader — an availability problem,
    /// never a silent-corruption one.
    ///
    /// The served-collection entry is recorded exactly the way `role_serve`
    /// records one, for the same two reasons: the re-import short-circuit
    /// (re-seeding an already-seeded package must not re-hash a multi-GB dir) and
    /// the by-hash ordered entry list. Attribution differs BY DESIGN — a
    /// `project/…` key has no `/pkg/` segment, so `resolve_served_root_in` yields
    /// `None` for it and a swarm GET (no announce, no ack claim, no transfer row
    /// behind it) never routes upload progress at a package id nothing is waiting
    /// on. That is a skip, not a mask: `find_map` walks past a `None`, so when a
    /// project seed and a role serve share one collection hash — a publisher
    /// push-seeds the very dir it just seeded — the role tag still resolves the
    /// transfer. The per-file resolver is indifferent to which of the two it
    /// picks: equal hashes mean byte-identical collections, hence identical
    /// ordered entry lists.
    pub async fn seed_project_collection(
        &self,
        project_id: &str,
        package_id: &str,
        pkg_dir: &Path,
    ) -> Result<Hash> {
        let tag = project_seed_tag(project_id, package_id);
        {
            let served = self.served.lock().expect("served mutex poisoned");
            if let Some(entry) = served.get(&tag) {
                tracing::debug!(
                    project_id,
                    package_id,
                    root_hash = %entry.hash,
                    "project seed reuses the already-imported collection"
                );
                return Ok(entry.hash);
            }
        }
        let (hash, entries) = blobs::import_package_collection_with_mode(
            &self.store,
            pkg_dir,
            &tag,
            ImportMode::TryReference,
        )
        .await
        .with_context(|| format!("seed project package {package_id}"))?;
        self.served.lock().expect("served mutex poisoned").insert(
            tag.clone(),
            ServedCollection {
                hash,
                want_fingerprint: None,
                src_dir: pkg_dir.to_path_buf(),
            },
        );
        self.served_files
            .lock()
            .expect("served_files mutex poisoned")
            .insert(tag, entries);
        tracing::info!(
            project_id,
            package_id,
            root_hash = %hash,
            path = %pkg_dir.display(),
            "project collection seeded"
        );
        Ok(hash)
    }

    /// Stop seeding ONE project package: forget its served entry and delete its
    /// `project/<project_id>/<package_id>` tag (exact name — a sibling package of
    /// the same project and every other project are untouched).
    ///
    /// Best-effort by design: this runs from local-data deletion paths, where a
    /// tag that refuses to go must not abort the deletion. The failure is logged,
    /// never swallowed; what it leaves behind is a pinned collection over files
    /// that are about to disappear — a failing GET, never a wrong one.
    pub async fn unseed_project_package(&self, project_id: &str, package_id: &str) {
        let tag = project_seed_tag(project_id, package_id);
        self.forget_served_tag(&tag);
        match self.store.tags().delete(tag.as_bytes()).await {
            Ok(removed) => tracing::info!(
                project_id,
                package_id,
                tags_removed = removed,
                "project package unseeded"
            ),
            Err(e) => tracing::warn!(
                project_id,
                package_id,
                error = %e,
                "delete project seed tag failed"
            ),
        }
    }

    /// Stop seeding EVERY package of one project — the prefix twin of
    /// [`unseed_project_package`](Self::unseed_project_package), for
    /// leave-project / delete-project-data. Scoped to `project/<project_id>/`, so
    /// another project's seeds survive. Same best-effort semantics.
    pub async fn unseed_project(&self, project_id: &str) {
        let prefix = project_seed_prefix(project_id);
        self.forget_served_prefix(&prefix);
        match self.store.tags().delete_prefix(prefix.as_bytes()).await {
            Ok(removed) => {
                tracing::info!(project_id, tags_removed = removed, "project unseeded")
            }
            Err(e) => tracing::warn!(
                project_id,
                error = %e,
                "delete project seed tags failed"
            ),
        }
    }

    /// Drop one tag's `served` + `served_files` entries in lock-step (the two maps
    /// never drift), the way `role_release` does for a role package tag.
    fn forget_served_tag(&self, tag: &str) {
        self.served
            .lock()
            .expect("served mutex poisoned")
            .remove(tag);
        self.served_files
            .lock()
            .expect("served_files mutex poisoned")
            .remove(tag);
    }

    /// [`forget_served_tag`](Self::forget_served_tag) for every key under `prefix`.
    fn forget_served_prefix(&self, prefix: &str) {
        self.served
            .lock()
            .expect("served mutex poisoned")
            .retain(|tag, _| !tag.starts_with(prefix));
        self.served_files
            .lock()
            .expect("served_files mutex poisoned")
            .retain(|tag, _| !tag.starts_with(prefix));
    }

    /// Enumerate the receiver-side in-flight download tags currently in the
    /// shared store as their wire [`PackageId`]s (B7 orphan reclaim). Lists ONLY
    /// the `in-flight/recv/pkg/` namespace via a prefixed tag scan and parses
    /// each name through [`recv_in_flight_package_id`], so a permanent
    /// `<role>/pkg/…` tag, a `project/…` tag, or any foreign tag is never
    /// surfaced — the namespace contract holds by construction. Best-effort per
    /// entry: a malformed tag name is skipped, never fatal.
    async fn role_list_in_flight_tags(&self) -> Result<Vec<PackageId>> {
        use n0_future::StreamExt as _;
        let mut ids = Vec::new();
        let mut stream = self
            .store
            .tags()
            .list_prefix(RECV_IN_FLIGHT_PREFIX.as_bytes())
            .await
            .map_err(|e| anyhow!("list in-flight tags: {e}"))?;
        while let Some(entry) = stream.next().await {
            let info = entry.map_err(|e| anyhow!("list in-flight tag entry: {e}"))?;
            let name = String::from_utf8_lossy(info.name.as_ref());
            if let Some(id) = recv_in_flight_package_id(&name) {
                ids.push(PackageId(id.to_string()));
            }
        }
        Ok(ids)
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
}

/// A per-[`Role`] view onto a [`SharedIrohNode`], implementing
/// [`SharingTransport`] with role-prefixed blob tags (Д3). Cheap to clone
/// (an `Arc` + a `Copy` role). The node it references is torn down by the host
/// via [`SharedIrohNode::shutdown`], not by any one handle.
pub struct RoleHandle {
    node: Arc<SharedIrohNode>,
    role: Role,
    /// Unique id so the demux can attribute this handle's ack claims + Recv
    /// consumer registration and release them all when the handle drops.
    id: u64,
    /// This handle's own events channel sender, set on the first
    /// [`events`](SharingTransport::events) call; an ack claim points at it.
    events_tx: Mutex<Option<mpsc::Sender<TransportEvent>>>,
}

impl RoleHandle {
    /// The shared node's blob store (used by tests to prove two role handles read
    /// the SAME store instance).
    #[allow(dead_code)] // consumed by the shared-store test below
    pub(crate) fn store(&self) -> &Store {
        self.node.store()
    }

    /// Register an ack claim for `(to, package_id)` pointing at this handle's
    /// events channel, so the eventual [`AckReceived`](TransportEvent::AckReceived)
    /// routes back here (Task 2). A no-op-with-warn if
    /// [`events`](SharingTransport::events) has not been taken yet (the ack would
    /// then have no claimant) — the engine always takes it first.
    fn claim_ack(&self, to: NodeId, package_id: &PackageId) {
        let tx = self
            .events_tx
            .lock()
            .expect("events_tx mutex poisoned")
            .clone();
        match tx {
            Some(tx) => self
                .node
                .demux
                .register_claim(self.id, (to, package_id.clone()), tx),
            None => tracing::warn!(
                package_id = %package_id.0,
                to = %hex32(&to),
                "announce before events() taken; ack will have no claimant"
            ),
        }
    }
}

impl Drop for RoleHandle {
    fn drop(&mut self) {
        // Release every claim + the Recv registration this handle owned, so
        // nothing routes to a dropped handle's dead channel (claims can't leak).
        self.node.demux.release_handle(self.id);
    }
}

#[async_trait]
impl SharingTransport for RoleHandle {
    async fn start(&self) -> Result<StartInfo> {
        self.node.role_start(self.role).await
    }

    /// Role-independent: a presence beacon says "this DEVICE is reachable", not
    /// "this role is", so every handle delegates to the one node.
    async fn send_presence(&self, to: NodeId) -> Result<()> {
        self.node.send_presence(to).await
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
        // Claim the eventual ack for THIS handle before announcing; release the
        // claim if the announce itself fails (claims can't leak on the error path).
        self.claim_ack(to, &a.package_id);
        let res = self
            .node
            .role_announce(self.role, to, a, batch_name, batch_uuid, files, layout)
            .await;
        if res.is_err() {
            self.node.demux.release_claim(&(to, a.package_id.clone()));
        }
        res
    }

    async fn revoke(&self, to: NodeId, package_id: &PackageId, reason: RevokeReason) -> Result<()> {
        // Best-effort control message; no ack-claim to manage (revoke never
        // correlates a return receipt). Routes to the shared node's send path.
        self.node.revoke_inner(to, package_id, reason).await
    }

    async fn fetch(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
        sink: FetchSink,
    ) -> Result<()> {
        self.node
            .role_fetch(self.role, from, pkg, dest_dir, sink)
            .await
    }

    async fn fetch_collection_multi(
        &self,
        providers: Vec<NodeId>,
        root_hash: &str,
        byte_size: u64,
        dest_dir: &Path,
        sink: FetchSink,
        telemetry: ProviderTelemetrySink,
    ) -> Result<()> {
        self.node
            .role_fetch_multi(
                self.role, providers, root_hash, byte_size, dest_dir, sink, telemetry,
            )
            .await
    }

    async fn fetch_manifest(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
    ) -> Result<PathBuf> {
        self.node.role_fetch_manifest(from, pkg, dest_dir).await
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
        // The wire announce's package_id is the ack-correlation id — claim on it.
        self.claim_ack(to, &announce.package_id);
        let res = self
            .node
            .role_announce_project(self.role, to, project_id, package_id, announce)
            .await;
        if res.is_err() {
            self.node
                .demux
                .release_claim(&(to, announce.package_id.clone()));
        }
        res
    }

    async fn request_project(&self, to: NodeId, project_id: &str, package_id: &str) -> Result<()> {
        self.node
            .role_request_project(to, project_id, package_id)
            .await
    }

    async fn release(&self, package_id: &PackageId) -> Result<()> {
        self.node.role_release(self.role, package_id).await
    }

    async fn protect_shared_before_cleanup(&self, package_id: &PackageId) -> Result<()> {
        self.node
            .role_protect_shared_before_cleanup(self.role, package_id)
            .await
    }

    async fn list_in_flight_tags(&self) -> Result<Vec<PackageId>> {
        self.node.role_list_in_flight_tags().await
    }

    fn add_peer_dial_hint(&self, from: NodeId) {
        // I2 (T7): route the blob-pull dial hint to the shared node's address
        // lookup (relay-only, our own relay set). The loopback transport keeps the
        // trait default no-op.
        self.node.add_peer_dial_hint(from);
    }

    fn add_peer_addr(&self, addr: EndpointAddr) {
        // T8 retry re-resolution: register the peer's refreshed address on the
        // shared node (peer book + address lookup), so the engine's next re-attempt
        // dials the peer's current relay/direct path.
        self.node.add_peer(addr);
    }

    fn remove_peer_addr(&self, peer: NodeId) {
        // Task 3.6: drop the peer's stale address-lookup entry so the engine's
        // immediately following `add_peer_addr` fully replaces (not merges onto) a
        // now-dead relay URL. Loopback keeps the trait default no-op.
        self.node.remove_peer_addr(peer);
    }

    async fn events(&self) -> mpsc::Receiver<TransportEvent> {
        // Task 2 demux: each handle gets its OWN channel. The Recv handle
        // registers as the single inbound consumer (announces/requests); an
        // Out/Collab handle stores its sender so its announces can claim the
        // matching ack. The engine takes this exactly once per handle.
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        *self.events_tx.lock().expect("events_tx mutex poisoned") = Some(tx.clone());
        if self.role == Role::Recv {
            self.node.demux.register_recv(self.id, tx);
        }
        rx
    }

    async fn shutdown(&self) {
        // A role handle is a view onto the shared node; the host owns the node's
        // lifecycle and calls `SharedIrohNode::shutdown` directly (Task 3 wiring).
        // Tearing the whole node down from one role handle would kill its
        // siblings, so this is intentionally a no-op.
    }
}

/// Compute the relay-url set difference for a hot-swap (Task 3.5): the urls to
/// `insert_relay` (present in `new`, absent from `old`) and the urls to
/// `remove_relay` (present in `old`, absent from `new`). Order-insensitive; a
/// pure set diff (any within-side duplicates are irrelevant).
fn relay_set_diff(old: &[String], new: &[String]) -> (Vec<String>, Vec<String>) {
    let old_set: HashSet<&str> = old.iter().map(String::as_str).collect();
    let new_set: HashSet<&str> = new.iter().map(String::as_str).collect();
    let added: Vec<String> = new
        .iter()
        .filter(|u| !old_set.contains(u.as_str()))
        .cloned()
        .collect();
    let removed: Vec<String> = old
        .iter()
        .filter(|u| !new_set.contains(u.as_str()))
        .cloned()
        .collect();
    (added, removed)
}

/// Spawn the transport-telemetry sampler: every [`TELEMETRY_INTERVAL`] it logs
/// the traffic of the window just closed (relay vs direct bytes, paths opened,
/// hole-punch attempts, home-relay changes) and skips the line entirely when
/// nothing happened, so an idle app writes nothing. Holds an endpoint clone, so
/// the node aborts it at shutdown like the relay watcher.
fn spawn_transport_telemetry(endpoint: Endpoint) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut previous = TransportCounters::snapshot(&endpoint);
        loop {
            tokio::time::sleep(TELEMETRY_INTERVAL).await;
            let now = TransportCounters::snapshot(&endpoint);
            let window = now.delta(&previous);
            previous = now;
            if !window.is_quiet() {
                window.log("tick");
            }
        }
    })
}

/// One home-relay status reduced to the three fields the watcher reasons about.
/// iroh's `RelayStatus` has no public constructor, so the decisions below are
/// pure functions over THIS shape and a test can build them directly.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RelayStatusView {
    url: String,
    connected: bool,
    last_error: Option<String>,
}

/// Reduce the whole home-relay status set to the node's reachability picture:
/// `(connected, url, last_error)`, connected iff ANY home relay is connected —
/// the same rule [`iroh::Endpoint::online`] applies. When nothing is connected
/// the reported url/error come from the first entry that carries an error, else
/// the first entry at all. See [`RelayHealth`] for why this is an aggregate.
fn aggregate_health(views: &[RelayStatusView]) -> (bool, Option<String>, Option<String>) {
    if let Some(up) = views.iter().find(|v| v.connected) {
        return (true, Some(up.url.clone()), None);
    }
    let down = views
        .iter()
        .find(|v| v.last_error.is_some())
        .or_else(|| views.first());
    (
        false,
        down.map(|v| v.url.clone()),
        down.and_then(|v| v.last_error.clone()),
    )
}

/// Whether a per-relay "not connected" observation deserves a `warn!`.
///
/// A relay that WAS connected and dropped is an anomaly, and so is one reporting
/// an error the very first time we see it (a relay refusing this device's key
/// lands exactly there — that must stay loud). What is NOT an anomaly is the
/// first, error-free sighting of a relay that simply has not finished connecting:
/// the watcher treats every first status as a transition, so that case used to
/// emit `WARN home relay disconnected` on every single app start and read as a
/// fault in a beta user's log.
fn disconnect_is_anomalous(prev: Option<bool>, connected: bool, has_error: bool) -> bool {
    !connected && (prev == Some(true) || has_error)
}

/// Spawn the home-relay status watcher (spec §1, minor #4). iroh's
/// [`home_relay_status`](iroh::Endpoint::home_relay_status) watcher surfaces relay
/// connectivity transitions — a relay eviction (`SameEndpointIdConnected`, the C1
/// symptom) shows up here as a disconnect instead of generic downstream timeouts.
/// We log only *transitions* (`info!` on connect, `warn!` on an anomalous
/// disconnect, `debug!` on a relay that has not connected yet) so a steady state
/// is silent. The task ends when the last endpoint clone drops; the node aborts it
/// at shutdown (closing the endpoint alone does not stop it).
///
/// The [`RelayHealth`] cell is recomputed from the WHOLE status set on every
/// update (see that type's docs), not from whichever entry changed — and only
/// written when the picture actually changes, so `since` keeps its meaning.
///
/// Coming online is a wake event (Task 6): the node just (re)became reachable, so
/// it fires the installed [`WakeHook`] to kick every pending outbound package out
/// of its backoff. The edge is taken on the AGGREGATE (no home relay → some home
/// relay), so re-homing between two live relays — which iroh does on its own — no
/// longer fires a redundant kick. `wake_hook` is the node's shared hook lock —
/// read (and cloned out from under) at FIRE time, never captured here at spawn —
/// so a hook installed after this task spawned is always the one that fires. A
/// relay-map hot-swap re-homes the SAME endpoint, so this one watcher observes
/// every relay-set change without being respawned.
fn spawn_home_relay_watcher(
    endpoint: Endpoint,
    wake_hook: Arc<RwLock<Option<WakeHook>>>,
    relay_health: Arc<RwLock<RelayHealth>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        use n0_future::StreamExt as _;
        let mut stream = endpoint.home_relay_status().stream();
        let mut last: HashMap<String, bool> = HashMap::new();
        let mut last_aggregate: Option<bool> = None;
        while let Some(statuses) = stream.next().await {
            let views: Vec<RelayStatusView> = statuses
                .iter()
                .map(|st| RelayStatusView {
                    url: st.url().to_string(),
                    connected: st.is_connected(),
                    last_error: st.last_error().map(|e| e.to_string()),
                })
                .collect();

            // Health surface (Task 3.3): aggregate over the whole set, rewritten
            // only when it differs from what the cell already holds. Written under
            // the lock, no await held; a poll reads the current picture.
            let (connected, url, last_error) = aggregate_health(&views);
            {
                let mut cell = relay_health.write().expect("relay_health lock poisoned");
                if cell.connected != connected || cell.url != url || cell.last_error != last_error {
                    *cell = RelayHealth {
                        connected,
                        url: url.clone(),
                        last_error: last_error.clone(),
                        since: Instant::now(),
                    };
                }
            }

            // Per-relay transition logging — the granularity that makes a relay
            // eviction or a refused key readable in the log.
            for v in &views {
                let prev = last.get(&v.url).copied();
                if prev == Some(v.connected) {
                    continue;
                }
                if v.connected {
                    tracing::info!(relay_url = %v.url, "home relay connected");
                } else if disconnect_is_anomalous(prev, v.connected, v.last_error.is_some()) {
                    match v.last_error.as_deref() {
                        Some(err) => {
                            tracing::warn!(relay_url = %v.url, error = %err, "home relay disconnected")
                        }
                        None => tracing::warn!(relay_url = %v.url, "home relay disconnected"),
                    }
                } else {
                    tracing::debug!(relay_url = %v.url, "home relay not connected yet");
                }
                last.insert(v.url.clone(), v.connected);
            }

            // Wake on the aggregate edge into "reachable".
            let became_reachable = connected && last_aggregate != Some(true);
            last_aggregate = Some(connected);
            if became_reachable {
                // Clone the hook out from under the lock, then invoke it with no
                // lock held (no reentrant deadlock).
                let hook = wake_hook.read().expect("wake_hook lock poisoned").clone();
                if let Some(h) = hook {
                    h();
                }
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

    /// A two-frame package with DELIBERATELY distinct payload sizes (Task 13
    /// regression test): a real iroh collection for even one frame already has
    /// multiple blobs (the hash-seq blob + the manifest + the frame payload), so
    /// two frames of different sizes give a clear, hard-to-fake signal that
    /// upload-progress reporting is truly cumulative across ALL of them — a
    /// per-blob-only bug would freeze at whichever single blob is largest
    /// (`size_b`, assuming `size_b > size_a` and both dwarf the hash-seq/manifest
    /// blobs), never reaching the collection's full announced `byte_size`.
    fn build_two_frame_package(
        base: &Path,
        size_a: usize,
        size_b: usize,
    ) -> (PathBuf, PackageAnnounce) {
        let src = base.join("src2");
        std::fs::create_dir_all(&src).unwrap();
        let mut records = Vec::new();
        for (i, (name, size)) in [("frame_a.fits", size_a), ("frame_b.fits", size_b)]
            .into_iter()
            .enumerate()
        {
            let payload = src.join(name);
            // Distinct, non-repeating content per file so a wrong-blob mixup would
            // also fail a hash check, not just a size check.
            let bytes: Vec<u8> = (0..size).map(|j| ((j + i * 97) % 251) as u8).collect();
            std::fs::write(&payload, &bytes).unwrap();
            let byte_size = std::fs::metadata(&payload).unwrap().len();
            let xxh3 = crate::package::xxh3_full_file(&payload).unwrap();
            records.push((
                payload,
                ManifestRecord {
                    v: MANIFEST_VERSION,
                    frame_uuid: format!("uuid-two-{i}"),
                    origin_catalog_uuid: "catalog-uuid".to_string(),
                    origin_device: "origin-device".to_string(),
                    payload_kind: PayloadKind::RawFrame,
                    rel_path: name.to_string(),
                    byte_size,
                    xxh3,
                    frame_meta: serde_json::json!({ "object": "M42" }),
                    analysis: None,
                    app_version: "test".to_string(),
                    project: None,
                },
            ));
        }
        let pkg_dir = base.join("pkg2");
        let announce = write_package(&pkg_dir, records).unwrap();
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

    /// D1 / audit F8: re-serving the same package must NOT re-import it. Every
    /// retry called `serve` first, and `role_serve` re-hashed the whole payload
    /// through `add_path` before the announce discovered the peer was absent —
    /// which is what made a frequent retry schedule unaffordable.
    ///
    /// Mutating the file on disk between the two serves is the proof: a real
    /// re-import would hash the NEW bytes and land a second blob.
    #[tokio::test]
    async fn re_serving_the_same_package_skips_the_import() {
        let dir = tempdir().unwrap();
        let node = SharedIrohNode::bind(dir.path(), RelayMode::Disabled)
            .await
            .unwrap();
        let (pkg_dir, announce) = build_one_frame_package(dir.path());
        let handle = node.role_handle(Role::Out);

        handle.serve(&announce, &pkg_dir, None).await.unwrap();
        let first_hash = node
            .resolve_served_hash_for_test(Role::Out, &announce.package_id)
            .expect("the first serve records a collection hash");

        std::fs::write(pkg_dir.join("frame.fits"), b"completely different bytes").unwrap();
        handle.serve(&announce, &pkg_dir, None).await.unwrap();
        let second_hash = node
            .resolve_served_hash_for_test(Role::Out, &announce.package_id)
            .expect("the second serve keeps a collection hash");

        assert_eq!(
            first_hash, second_hash,
            "the second serve reused the already-imported collection instead of \
             re-hashing the (now different) payload"
        );

        // Releasing forgets the collection, so a rebuilt payload — Perseus's resend
        // path — imports for real again rather than serving stale bytes.
        handle.release(&announce.package_id).await.unwrap();
        handle.serve(&announce, &pkg_dir, None).await.unwrap();
        let after_release = node
            .resolve_served_hash_for_test(Role::Out, &announce.package_id)
            .expect("the post-release serve records a hash");
        assert_ne!(
            after_release, first_hash,
            "after release the changed payload must be imported for real"
        );

        node.shutdown().await;
    }

    /// The short-circuit keys on the want-subset too, not the tag alone: a second
    /// serve of the SAME package with a DIFFERENT negotiated subset is a different
    /// collection, and reusing the first one would advertise a root missing the
    /// frames the peer just asked for.
    #[tokio::test]
    async fn a_different_want_subset_is_imported_rather_than_reused() {
        let dir = tempdir().unwrap();
        let node = SharedIrohNode::bind(dir.path(), RelayMode::Disabled)
            .await
            .unwrap();
        let (pkg_dir, announce) = build_two_frame_package(dir.path(), 1024, 2048);
        let handle = node.role_handle(Role::Out);

        let full = HashSet::from(["frame_a.fits".to_string(), "frame_b.fits".to_string()]);
        handle
            .serve(&announce, &pkg_dir, Some(&full))
            .await
            .unwrap();
        let full_hash = node
            .resolve_served_hash_for_test(Role::Out, &announce.package_id)
            .expect("full-subset serve records a hash");

        let subset = HashSet::from(["frame_a.fits".to_string()]);
        handle
            .serve(&announce, &pkg_dir, Some(&subset))
            .await
            .unwrap();
        let subset_hash = node
            .resolve_served_hash_for_test(Role::Out, &announce.package_id)
            .expect("subset serve records a hash");

        assert_ne!(
            full_hash, subset_hash,
            "a narrower want must produce its own collection, never reuse the wider one"
        );

        // And the SAME subset again does reuse — the fingerprint compares by
        // content, not by the HashSet's iteration order.
        let same_subset_again = HashSet::from(["frame_a.fits".to_string()]);
        handle
            .serve(&announce, &pkg_dir, Some(&same_subset_again))
            .await
            .unwrap();
        assert_eq!(
            node.resolve_served_hash_for_test(Role::Out, &announce.package_id),
            Some(subset_hash),
            "an identical subset reuses the import"
        );

        node.shutdown().await;
    }

    /// Total bytes of every regular file under `dir` (D3 T2 store-growth oracle).
    fn dir_size(dir: &Path) -> u64 {
        walkdir::WalkDir::new(dir)
            .into_iter()
            .flatten()
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| e.metadata().ok().map(|m| m.len()))
            .sum()
    }

    /// D3 T2: a project seed lands under the RESERVED `project/…` namespace and
    /// survives every role's startup sweep. The sweep is what makes seeding
    /// durable at all — a seeding tag that a sibling role's boot deleted would
    /// turn every seeded device into a phantom holder on the next app start.
    #[tokio::test]
    async fn seed_tag_lands_and_survives_the_sweep() {
        let dir = tempdir().unwrap();
        let node = SharedIrohNode::bind(dir.path(), RelayMode::Disabled)
            .await
            .unwrap();
        let (pkg_dir, _announce) = build_one_frame_package(dir.path());

        node.seed_project_collection("proj-1", "pkg-1", &pkg_dir)
            .await
            .expect("seeding a retained package dir succeeds");

        assert!(
            tag_present(node.store(), "project/proj-1/pkg-1").await,
            "the seed lands under project/<project_id>/<package_id>"
        );

        // A control tag in a swept namespace proves the sweeps actually ran.
        let tt = node
            .store()
            .blobs()
            .add_bytes(b"sweep control".to_vec())
            .temp_tag()
            .await
            .unwrap();
        node.store()
            .tags()
            .set("out/pkg/control", tt.hash_and_format())
            .await
            .unwrap();
        drop(tt);
        for role in [Role::Recv, Role::Out, Role::Collab] {
            node.handle(role).start().await.unwrap();
        }
        assert!(
            !tag_present(node.store(), "out/pkg/control").await,
            "the control tag in the swept namespace is gone (the sweeps ran)"
        );
        assert!(
            tag_present(node.store(), "project/proj-1/pkg-1").await,
            "the project seed survives every role's startup sweep"
        );

        node.shutdown().await;
    }

    /// D3 T2: seeding imports with `ImportMode::TryReference`, so the blobs
    /// reference the retained package dir in place — a seeding device pays
    /// metadata, not a second copy of every frame. Compared against the same
    /// package imported the normal (Copy) way by `serve`, in its own store.
    ///
    /// The oracle is the store's OWNED-DATA dir (`<blobs>/data`, where a copied
    /// blob's `<hash>.data` lands), not the whole store dir: the redb index file
    /// is ~1 MB the moment it exists and would drown the signal.
    #[tokio::test]
    async fn try_reference_does_not_copy_payloads() {
        const PAYLOAD: usize = 1024 * 1024;
        // The package lives OUTSIDE both store dirs, so measuring the store
        // measures only what the store itself wrote.
        let pkg_root = tempdir().unwrap();
        let (pkg_dir, announce) = build_two_frame_package(pkg_root.path(), PAYLOAD, 4096);

        let copy_dir = tempdir().unwrap();
        let copy_node = SharedIrohNode::bind(copy_dir.path(), RelayMode::Disabled)
            .await
            .unwrap();
        copy_node
            .role_handle(Role::Out)
            .serve(&announce, &pkg_dir, None)
            .await
            .unwrap();
        copy_node.shutdown().await;
        let copied = dir_size(&copy_dir.path().join("blobs").join("data"));

        let ref_dir = tempdir().unwrap();
        let ref_node = SharedIrohNode::bind(ref_dir.path(), RelayMode::Disabled)
            .await
            .unwrap();
        ref_node
            .seed_project_collection("proj-1", "pkg-1", &pkg_dir)
            .await
            .unwrap();
        ref_node.shutdown().await;
        let referenced = dir_size(&ref_dir.path().join("blobs").join("data"));

        assert!(
            copied as usize >= PAYLOAD,
            "sanity: a Copy import writes the payload into the store \
             (measured {copied} bytes for a {PAYLOAD}-byte payload)"
        );
        assert!(
            referenced < 256 * 1024,
            "a TryReference import must not duplicate the payload: the store grew \
             by {referenced} bytes (Copy grew by {copied})"
        );
    }

    /// D3 T2: the unseed API is exactly as scoped as its name — per package by
    /// exact tag, per project by prefix — so tearing one project's local data
    /// down can never stop another project's packages from being served.
    #[tokio::test]
    async fn unseed_removes_exactly_the_project_tags() {
        let dir = tempdir().unwrap();
        let node = SharedIrohNode::bind(dir.path(), RelayMode::Disabled)
            .await
            .unwrap();
        let (pkg_dir, _announce) = build_one_frame_package(dir.path());

        for (project, package) in [
            ("proj-1", "pkg-a"),
            ("proj-1", "pkg-b"),
            ("proj-2", "pkg-z"),
        ] {
            node.seed_project_collection(project, package, &pkg_dir)
                .await
                .unwrap();
        }

        node.unseed_project_package("proj-1", "pkg-a").await;
        assert!(
            !tag_present(node.store(), "project/proj-1/pkg-a").await,
            "the named package is unseeded"
        );
        assert!(
            tag_present(node.store(), "project/proj-1/pkg-b").await,
            "a sibling package in the same project keeps seeding"
        );
        assert!(
            tag_present(node.store(), "project/proj-2/pkg-z").await,
            "another project is untouched"
        );

        node.unseed_project("proj-1").await;
        assert!(
            !tag_present(node.store(), "project/proj-1/pkg-b").await,
            "unseeding the project removes its remaining packages"
        );
        assert!(
            tag_present(node.store(), "project/proj-2/pkg-z").await,
            "unseeding one project never touches another's tags"
        );

        node.shutdown().await;
    }

    /// D1: a presence beacon reaches the peer's installed hook, carrying the
    /// AUTHENTICATED sender id. The message has no payload precisely so that the
    /// identity can only come from the connection — this asserts that path.
    #[tokio::test]
    async fn presence_beacon_fires_the_peers_hook_with_the_authenticated_id() {
        let dir = tempdir().unwrap();
        // The SENDER installs the hook: it is the side parked on a retry, waiting
        // to hear that its peer is back.
        let sender = SharedIrohNode::bind(&dir.path().join("sender"), RelayMode::Disabled)
            .await
            .unwrap();
        let receiver = SharedIrohNode::bind(&dir.path().join("receiver"), RelayMode::Disabled)
            .await
            .unwrap();

        let seen: Arc<Mutex<Vec<NodeId>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let seen = Arc::clone(&seen);
            sender.set_presence_hook(Arc::new(move |from| {
                seen.lock().expect("seen mutex poisoned").push(from);
            }));
        }

        // Relay-disabled endpoints have no discovery, so each side needs the
        // other's address out of band — the same ticket exchange the transport
        // tests use.
        let sender_info = sender.handle(Role::Out).start().await.unwrap();
        let receiver_info = receiver.handle(Role::Recv).start().await.unwrap();
        sender
            .add_peer_ticket(&receiver_info.pairing_ticket)
            .unwrap();
        receiver
            .add_peer_ticket(&sender_info.pairing_ticket)
            .unwrap();

        receiver
            .send_presence(sender.node_id())
            .await
            .expect("beacon delivers");

        for _ in 0..200 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[receiver.node_id()],
            "the hook fires exactly once, with the beacon sender's authenticated id"
        );

        sender.shutdown().await;
        receiver.shutdown().await;
    }

    /// Review finding (critical): the fan-out must supply its own dial hint.
    ///
    /// `node.peers` is populated only on the SEND path (`ensure_sender_engine`
    /// registers the destination when it builds an engine). A beacon goes out from
    /// the RECEIVING half, to devices we may never have sent to — so with no hint
    /// of its own `send_presence` bails locally on "no known address" and the
    /// beacon never leaves the machine. This pins the contract at the transport
    /// level: given a registered address, an UNSEEN peer is dialable.
    #[tokio::test]
    async fn a_registered_hint_makes_an_otherwise_unknown_peer_dialable() {
        let dir = tempdir().unwrap();
        let sender = SharedIrohNode::bind(&dir.path().join("s"), RelayMode::Disabled)
            .await
            .unwrap();
        let receiver = SharedIrohNode::bind(&dir.path().join("r"), RelayMode::Disabled)
            .await
            .unwrap();

        let seen: Arc<Mutex<Vec<NodeId>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let seen = Arc::clone(&seen);
            sender.set_presence_hook(Arc::new(move |from| {
                seen.lock().expect("seen mutex poisoned").push(from);
            }));
        }
        let sender_info = sender.handle(Role::Out).start().await.unwrap();
        receiver.handle(Role::Recv).start().await.unwrap();

        // The receiver has NEVER sent to this peer, so its peer book is empty:
        // exactly the production shape the beacon runs in.
        assert!(
            receiver.send_presence(sender.node_id()).await.is_err(),
            "without a hint the beacon cannot dial — this is the bug the fan-out fix addresses"
        );

        // Registering the address — what `broadcast_presence` now does before every
        // beacon — makes the same call succeed.
        receiver
            .add_peer_ticket(&sender_info.pairing_ticket)
            .unwrap();
        receiver
            .send_presence(sender.node_id())
            .await
            .expect("beacon delivers with a hint");

        for _ in 0..200 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(seen.lock().unwrap().len(), 1, "the beacon arrived once");

        sender.shutdown().await;
        receiver.shutdown().await;
    }

    /// A beacon to a peer we have no address for must fail FAST rather than spend    /// A beacon to a peer we have no address for must fail FAST rather than spend
    /// the timeout — a fan-out runs across every account device, and one
    /// unaddressable peer must not hold up the rest.
    #[tokio::test]
    async fn presence_to_an_unknown_peer_fails_without_spending_the_timeout() {
        let dir = tempdir().unwrap();
        let node = SharedIrohNode::bind(dir.path(), RelayMode::Disabled)
            .await
            .unwrap();

        let started = Instant::now();
        let err = node
            .send_presence([9u8; 32])
            .await
            .expect_err("no address, no beacon");

        assert!(
            started.elapsed() < PRESENCE_SEND_TIMEOUT,
            "an unaddressable peer must not cost the full beacon timeout, took {:?}",
            started.elapsed()
        );
        assert!(
            format!("{err:#}").contains("no known address"),
            "the error names the cause: {err:#}"
        );
        node.shutdown().await;
    }

    // (b2) B7 namespace contract: `list_in_flight_tags` enumerates ONLY the
    //      receiver in-flight namespace (`in-flight/recv/pkg/<id>`) and `release`
    //      deletes ONLY that id's own tags — a permanent `<role>/pkg/…` tag and a
    //      foreign `project/…` tag survive both, so the B7 sweep/cleanup (which
    //      reach the store only through these two primitives) can never touch a
    //      tag the transfer machinery does not own.
    #[tokio::test]
    async fn in_flight_tag_list_and_release_honor_the_transfer_namespace() {
        let dir = tempdir().unwrap();
        let node = SharedIrohNode::bind(dir.path(), RelayMode::Disabled)
            .await
            .unwrap();
        let store = node.store();
        let tt = store
            .blobs()
            .add_bytes(b"seed".to_vec())
            .temp_tag()
            .await
            .unwrap();
        for name in [
            "in-flight/recv/pkg/orphan", // orphan in-flight → enumerated, released
            "in-flight/recv/pkg/live",   // in-flight → enumerated, kept
            "recv/pkg/perm",             // permanent receiver tag → NOT in-flight
            "out/pkg/x",                 // sender permanent tag
            "project/seed/deadbeef",     // FOREIGN (future project seeding) → must survive
        ] {
            store.tags().set(name, tt.hash_and_format()).await.unwrap();
        }
        drop(tt);

        let recv = node.handle(Role::Recv);

        // list surfaces EXACTLY the two in-flight wire ids — nothing else.
        let mut ids: Vec<String> = recv
            .list_in_flight_tags()
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.0)
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["live".to_string(), "orphan".to_string()],
            "list_in_flight_tags returns only the in-flight/recv/pkg/ namespace"
        );

        // Release the orphan: only its in-flight tag goes; everything else survives.
        recv.release(&PackageId("orphan".into())).await.unwrap();
        assert!(
            !tag_present(store, "in-flight/recv/pkg/orphan").await,
            "the orphan in-flight tag is released"
        );
        assert!(
            tag_present(store, "in-flight/recv/pkg/live").await,
            "the other in-flight tag is untouched"
        );
        assert!(
            tag_present(store, "recv/pkg/perm").await,
            "a permanent recv tag survives"
        );
        assert!(
            tag_present(store, "out/pkg/x").await,
            "a sender permanent tag survives"
        );
        assert!(
            tag_present(store, "project/seed/deadbeef").await,
            "a foreign project/… tag survives both list and release (B7 namespace contract)"
        );

        node.shutdown().await;
    }

    // B7: the in-flight prefix constant matches the tag constructors, so the sweep
    // and the fetch-path writer can never drift.
    #[test]
    fn recv_in_flight_prefix_matches_constructors() {
        assert_eq!(
            RECV_IN_FLIGHT_PREFIX,
            super::blobs::in_flight_tag(&format!("{}/pkg/", Role::Recv.prefix())),
        );
        // The parser round-trips a real receiver in-flight tag and rejects foreign
        // / permanent / malformed names.
        let tag = super::blobs::in_flight_tag(&role_package_tag(
            Role::Recv.prefix(),
            &PackageId("abc".into()),
        ));
        assert_eq!(recv_in_flight_package_id(&tag), Some("abc"));
        assert_eq!(recv_in_flight_package_id("recv/pkg/abc"), None);
        assert_eq!(recv_in_flight_package_id("out/pkg/abc"), None);
        assert_eq!(recv_in_flight_package_id("project/x/hash"), None);
        assert_eq!(recv_in_flight_package_id("in-flight/recv/pkg/"), None);
        assert_eq!(recv_in_flight_package_id("in-flight/recv/pkg/a/b"), None);
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

    fn view(url: &str, connected: bool, err: Option<&str>) -> RelayStatusView {
        RelayStatusView {
            url: url.to_string(),
            connected,
            last_error: err.map(str::to_string),
        }
    }

    #[test]
    fn health_is_connected_when_any_relay_is_connected() {
        let (connected, url, err) = aggregate_health(&[
            view("https://a.example", false, Some("timeout")),
            view("https://b.example", true, None),
        ]);
        assert!(connected);
        assert_eq!(url.as_deref(), Some("https://b.example"));
        assert_eq!(err, None, "a connected node reports no failure");
    }

    #[test]
    fn secondary_relay_drop_does_not_flip_health_to_direct_only() {
        // iroh re-homes between relays on its own; the drop of the one we are
        // leaving must not report the node unreachable while the other is up.
        let (connected, url, _) = aggregate_health(&[
            view("https://relay-ams.example", false, Some("closed")),
            view("https://relay2.example", true, None),
        ]);
        assert!(connected, "another home relay is connected");
        assert_eq!(url.as_deref(), Some("https://relay2.example"));
    }

    #[test]
    fn health_reports_the_failing_relay_when_nothing_is_connected() {
        let (connected, url, err) = aggregate_health(&[
            view("https://a.example", false, None),
            view("https://b.example", false, Some("refused")),
        ]);
        assert!(!connected);
        assert_eq!(url.as_deref(), Some("https://b.example"));
        assert_eq!(err.as_deref(), Some("refused"));
    }

    #[test]
    fn health_of_an_empty_status_set_is_disconnected_without_a_url() {
        let (connected, url, err) = aggregate_health(&[]);
        assert!(!connected);
        assert_eq!(url, None);
        assert_eq!(err, None);
    }

    #[test]
    fn first_error_free_disconnected_status_is_not_a_warning() {
        // The startup case: a relay we have never seen connected yet, with no
        // error — it is simply still connecting.
        assert!(!disconnect_is_anomalous(None, false, false));
    }

    #[test]
    fn drop_after_a_connect_is_a_warning() {
        assert!(disconnect_is_anomalous(Some(true), false, false));
    }

    #[test]
    fn first_status_carrying_an_error_is_a_warning() {
        // A relay refusing this device's key shows up as a first-sighting
        // disconnect WITH an error — that must stay loud.
        assert!(disconnect_is_anomalous(None, false, true));
    }

    // Task 3.3: a relay-disabled node has no home relay, so its transport health
    // is `direct_only` (undialable behind NAT) — never `relay_connected`. This is
    // the node-level half of the health derivation (the api layer owns
    // `not_started` / `no_relay_map`). No role is started and no relay is
    // configured, so the online-wait never runs and the watcher records nothing —
    // the bind-time `direct_only` baseline stands.
    #[tokio::test]
    async fn transport_health_relay_disabled_is_direct_only() {
        let dir = tempdir().unwrap();
        let node = bind_disabled(dir.path()).await;
        let health = node.transport_health();
        assert_eq!(
            health.status, "direct_only",
            "relay-disabled node is direct-only"
        );
        assert!(
            health.relay_url.is_none(),
            "no relay configured => no relay url"
        );
        node.shutdown().await;
    }

    // Task 3.3 regression: a relay-configured node that reports connected (the
    // online-wait seed / the watcher's first connect transition) must flip to
    // `direct_only` the moment the watcher records a DISCONNECT — the home relay
    // dropped. This is the exact path the old `online_ok` one-shot latch masked:
    // `health.connected || online_ok` stayed true forever after the single
    // successful `online()` wait, so a later relay drop still read
    // `relay_connected`. With `online_ok` gone, the `RelayHealth` cell is the sole
    // authority, so the disconnect transition wins.
    #[tokio::test]
    async fn transport_health_relay_drop_flips_to_direct_only() {
        let dir = tempdir().unwrap();
        let node = bind_disabled(dir.path()).await;
        // Loopback binds are relay-disabled; force the relay-configured branch so
        // the derivation depends on the health cell (not the disabled short-circuit).
        node.force_uses_relay_for_test(true);

        // 1) Connected: seeded by the successful online-wait / watcher's first
        //    connect transition.
        node.set_relay_health_for_test(RelayHealth {
            connected: true,
            url: Some("https://relay.example".to_string()),
            last_error: None,
            since: Instant::now(),
        });
        assert_eq!(
            node.transport_health().status,
            "relay_connected",
            "a connected relay reads relay_connected"
        );

        // 2) The watcher then records a DISCONNECT (home relay dropped).
        node.set_relay_health_for_test(RelayHealth {
            connected: false,
            url: Some("https://relay.example".to_string()),
            last_error: Some("relay closed".to_string()),
            since: Instant::now(),
        });
        let health = node.transport_health();
        assert_eq!(
            health.status, "direct_only",
            "a dropped relay must flip the surface to direct_only (regression: the \
             online_ok latch reported relay_connected forever)"
        );
        assert_eq!(
            health.last_error.as_deref(),
            Some("relay closed"),
            "the disconnect's last_error is surfaced"
        );

        node.shutdown().await;
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

    // Task 13: a served collection's root hash resolves back to its package id
    // (the reverse lookup the provider-upload-events consumer labels progress
    // with); an unknown hash resolves to None.
    #[tokio::test]
    async fn resolve_served_root_maps_collection_hash_to_package_id() {
        let dir = tempdir().unwrap();
        let node = SharedIrohNode::bind(dir.path(), RelayMode::Disabled)
            .await
            .unwrap();

        let out = node.role_handle(Role::Out);
        let (pkg_dir, announce) = build_one_frame_package(dir.path());
        out.serve(&announce, &pkg_dir, None).await.unwrap();

        // The collection root hash the serve registered, read back off its tag.
        let tag = format!("out/pkg/{}", announce.package_id.0);
        let root = node
            .store()
            .tags()
            .get(tag.as_bytes())
            .await
            .unwrap()
            .expect("served tag present")
            .hash;

        assert_eq!(
            node.resolve_served_root(root),
            Some(announce.package_id.clone()),
            "the served collection root must resolve back to its package id"
        );
        assert_eq!(
            node.resolve_served_root(Hash::new(b"an unserved blob")),
            None,
            "an unknown hash must resolve to None (child blob / foreign / hash-seq internal)"
        );

        node.shutdown().await;
    }

    // Task 13 fix (reviewer-caught regression): over a REAL iroh transfer, a
    // multi-blob collection's cumulative upload progress must reach the FULL
    // announced byte_size, not freeze at the single largest blob's size. Two
    // real `SharedIrohNode`s, relay disabled (localhost direct addresses, no
    // mock) — this exercises the actual `iroh_blobs::provider::events` stream
    // and the `UploadAccumulator` wired into `build_router`'s consumer, pinning
    // the fix against the real provider API (not just the pure accumulator unit
    // tests above).
    #[tokio::test]
    async fn serve_progress_over_real_iroh_accumulates_across_collection_blobs() {
        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r = bind_disabled(dr.path()).await;

        let out = s.handle(Role::Out);
        let recv = r.handle(Role::Recv);
        let s_info = out.start().await.unwrap();
        let r_info = recv.start().await.unwrap();
        pair(&s, &s_info, &r, &r_info);

        // Two frames of clearly different sizes (4 KiB, 64 KiB) so a per-blob
        // (not cumulative) bug would visibly undershoot the full byte_size.
        let (pkg_dir, announce) = build_two_frame_package(ds.path(), 4 * 1024, 64 * 1024);
        out.serve(&announce, &pkg_dir, None).await.unwrap();

        // Register both handles' consumers BEFORE announcing (same ordering the
        // other demux tests use): `out.events()` registers the ack-claim channel
        // that `route_serve_progress` will target once `announce` claims it.
        let mut out_events = out.events().await;
        let mut r_ev = recv.events().await;

        out.announce(r_info.node_id, &announce, "", "", &[], PackageLayout::Batch)
            .await
            .unwrap();
        let wire = match recv_next(&mut r_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce,
            other => panic!("expected AnnounceReceived, got {other:?}"),
        };

        // A real blob download over QUIC: this is what drives the provider's
        // `GetRequestReceivedNotify` → our consumer → `ServeProgress` ticks.
        let dest = tempdir().unwrap();
        recv.fetch(
            s_info.node_id,
            &wire,
            dest.path(),
            crate::sharing::noop_fetch_sink(),
        )
        .await
        .unwrap();

        // Poll for ServeProgress ticks until the peak reaches the full collection
        // size (the consumer's close-time flush may land slightly after `fetch`
        // returns — it runs on a detached task independent of the receiver's
        // completion) or the bound elapses.
        let mut peak = 0u64;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while peak < announce.byte_size && std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match tokio::time::timeout(remaining, out_events.recv()).await {
                Ok(Some(TransportEvent::ServeProgress { bytes_sent, .. })) => {
                    peak = peak.max(bytes_sent);
                }
                // The terminal `ServeComplete` (Task 2.1) and the per-file
                // `ServeFileProgress` ticks (Task 2.2) ride the same channel
                // interleaved with the batch progress — skip them, don't treat one
                // as an early break, so a timing quirk can't truncate the poll.
                Ok(Some(TransportEvent::ServeComplete { .. }))
                | Ok(Some(TransportEvent::ServeFileProgress { .. })) => continue,
                Ok(Some(_)) | Ok(None) => break,
                Err(_) => break, // overall deadline elapsed
            }
        }

        assert!(
            peak >= announce.byte_size,
            "cumulative upload progress must reach the full collection size ({}), got peak={peak} \
             (a per-blob-only regression would freeze at the largest single frame's size, ~64KiB)",
            announce.byte_size
        );

        s.shutdown().await;
        r.shutdown().await;
    }

    /// Collect every [`TransportEvent`] arriving on `rx` until it goes quiet for
    /// `quiet` (no new event within that window) or the overall `max` elapses.
    /// A quiet-window drain lets a NEGATIVE assertion (nothing arrives) return
    /// promptly while still bounding a POSITIVE one.
    async fn collect_until_quiet(
        rx: &mut Receiver<TransportEvent>,
        quiet: Duration,
        max: Duration,
    ) -> Vec<TransportEvent> {
        let mut out = Vec::new();
        let deadline = tokio::time::Instant::now() + max;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            let wait = quiet.min(deadline - now);
            match tokio::time::timeout(wait, rx.recv()).await {
                Ok(Some(ev)) => out.push(ev),
                Ok(None) => break, // channel closed
                Err(_) => break,   // quiet window elapsed with no new event
            }
        }
        out
    }

    // Task 2.1: a payload-carrying full pull emits EXACTLY ONE `ServeComplete`
    // (the terminal `Completed` of the phase-2 payload request), while a
    // `fetch_manifest` probe emits NEITHER a `ServeComplete` NOR any
    // `ServeProgress` tick — its only requests are the phase-1 root+meta pull
    // (resolves to the package but is NOT payload-carrying) and the manifest's
    // own raw blob (a different root the resolver maps to None). The full pull at
    // the end proves the provider-event plumbing is live, so the probe's silence
    // is a meaningful assertion, not a vacuous pass. Two real nodes, relay off.
    #[tokio::test]
    async fn manifest_probe_is_silent_and_full_pull_fires_one_serve_complete() {
        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r = bind_disabled(dr.path()).await;

        let out = s.handle(Role::Out);
        let recv = r.handle(Role::Recv);
        let s_info = out.start().await.unwrap();
        let r_info = recv.start().await.unwrap();
        pair(&s, &s_info, &r, &r_info);

        let (pkg_dir, announce) = build_two_frame_package(ds.path(), 4 * 1024, 64 * 1024);
        out.serve(&announce, &pkg_dir, None).await.unwrap();

        // Register consumers BEFORE announcing so the ack-claim channel exists for
        // route_serve_progress / route_serve_complete to target.
        let mut out_events = out.events().await;
        let mut r_ev = recv.events().await;
        out.announce(r_info.node_id, &announce, "", "", &[], PackageLayout::Batch)
            .await
            .unwrap();
        let wire = match recv_next(&mut r_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce,
            other => panic!("expected AnnounceReceived, got {other:?}"),
        };

        // (a) Manifest probe: NO ServeProgress, NO ServeComplete.
        let mdest = tempdir().unwrap();
        recv.fetch_manifest(s_info.node_id, &wire, mdest.path())
            .await
            .unwrap();
        let probe = collect_until_quiet(
            &mut out_events,
            Duration::from_millis(1500),
            Duration::from_secs(4),
        )
        .await;
        assert!(
            !probe
                .iter()
                .any(|e| matches!(e, TransportEvent::ServeComplete { .. })),
            "a manifest probe must not emit ServeComplete, got {probe:?}"
        );
        assert!(
            !probe
                .iter()
                .any(|e| matches!(e, TransportEvent::ServeProgress { .. })),
            "a manifest probe must not emit a ServeProgress tick, got {probe:?}"
        );

        // (b) Full pull of the payload: EXACTLY ONE ServeComplete.
        let fdest = tempdir().unwrap();
        recv.fetch(
            s_info.node_id,
            &wire,
            fdest.path(),
            crate::sharing::noop_fetch_sink(),
        )
        .await
        .unwrap();
        let pull = collect_until_quiet(
            &mut out_events,
            Duration::from_millis(1500),
            Duration::from_secs(10),
        )
        .await;
        let completes = pull
            .iter()
            .filter(|e| matches!(e, TransportEvent::ServeComplete { .. }))
            .count();
        assert_eq!(
            completes, 1,
            "a full payload pull must emit exactly one ServeComplete, got {completes} in {pull:?}"
        );
        assert!(
            pull.iter().any(|e| matches!(
                e,
                TransportEvent::ServeComplete { package_id } if *package_id == announce.package_id
            )),
            "ServeComplete must carry the served package id"
        );

        s.shutdown().await;
        r.shutdown().await;
    }

    /// A three-frame package with TWO BYTE-IDENTICAL files (Task 2.2 duplicate-hash
    /// attribution test): `a.fits` and `c.fits` carry identical content — so they
    /// share ONE blob hash — while `b.fits` differs. In the sorted collection they
    /// occupy distinct hash-seq indices (a → offset 2, b → 3, c → 4), so per-file
    /// upload attribution must resolve each to its OWN rel_path BY INDEX, never
    /// collapse the two onto one by their shared hash. Returns
    /// `(pkg_dir, announce, [(rel_path, size); 3])`.
    fn build_three_frame_dup_package(
        base: &Path,
    ) -> (PathBuf, PackageAnnounce, Vec<(String, u64)>) {
        let src = base.join("src3");
        std::fs::create_dir_all(&src).unwrap();
        // a.fits and c.fits are byte-identical (⇒ one shared blob hash); b differs.
        let dup: Vec<u8> = (0..16 * 1024).map(|j| (j % 251) as u8).collect();
        let mid: Vec<u8> = (0..32 * 1024).map(|j| ((j + 7) % 251) as u8).collect();
        let specs: [(&str, &Vec<u8>); 3] = [("a.fits", &dup), ("b.fits", &mid), ("c.fits", &dup)];
        let mut records = Vec::new();
        let mut expected = Vec::new();
        for (i, (name, bytes)) in specs.into_iter().enumerate() {
            let payload = src.join(name);
            std::fs::write(&payload, bytes).unwrap();
            let byte_size = std::fs::metadata(&payload).unwrap().len();
            let xxh3 = crate::package::xxh3_full_file(&payload).unwrap();
            records.push((
                payload,
                ManifestRecord {
                    v: MANIFEST_VERSION,
                    frame_uuid: format!("uuid-dup-{i}"),
                    origin_catalog_uuid: "catalog-uuid".to_string(),
                    origin_device: "origin-device".to_string(),
                    payload_kind: PayloadKind::RawFrame,
                    rel_path: name.to_string(),
                    byte_size,
                    xxh3,
                    frame_meta: serde_json::json!({ "object": "M42" }),
                    analysis: None,
                    app_version: "test".to_string(),
                    project: None,
                },
            ));
            expected.push((name.to_string(), byte_size));
        }
        let pkg_dir = base.join("pkg3");
        let announce = write_package(&pkg_dir, records).unwrap();
        (pkg_dir, announce, expected)
    }

    // Task 2.2: per-FILE upload attribution over a REAL iroh transfer. A 3-file
    // package with two byte-identical files (`a.fits` == `c.fits`, ONE shared blob
    // hash) must still produce a per-file `ServeFileProgress` bar for ALL THREE
    // rel_paths, each ending at its own size — because attribution is by hash-seq
    // INDEX, never by hash (which would collapse the two duplicates onto one). Two
    // real `SharedIrohNode`s, relay off; exercises the real provider-events stream
    // + the `ServeFileResolver`/`ServeFileTracker` wired into `build_router`.
    #[tokio::test]
    async fn per_file_upload_progress_attributes_by_index_incl_duplicate_hash() {
        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r = bind_disabled(dr.path()).await;

        let out = s.handle(Role::Out);
        let recv = r.handle(Role::Recv);
        let s_info = out.start().await.unwrap();
        let r_info = recv.start().await.unwrap();
        pair(&s, &s_info, &r, &r_info);

        let (pkg_dir, announce, expected) = build_three_frame_dup_package(ds.path());
        out.serve(&announce, &pkg_dir, None).await.unwrap();

        // Register consumers BEFORE announcing so the ack-claim channel exists for
        // route_serve_file_progress to target.
        let mut out_events = out.events().await;
        let mut r_ev = recv.events().await;
        out.announce(r_info.node_id, &announce, "", "", &[], PackageLayout::Batch)
            .await
            .unwrap();
        let wire = match recv_next(&mut r_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce,
            other => panic!("expected AnnounceReceived, got {other:?}"),
        };

        let dest = tempdir().unwrap();
        recv.fetch(
            s_info.node_id,
            &wire,
            dest.path(),
            crate::sharing::noop_fetch_sink(),
        )
        .await
        .unwrap();

        // Drain the provider's per-file ticks after the pull settles.
        let evs = collect_until_quiet(
            &mut out_events,
            Duration::from_millis(1500),
            Duration::from_secs(10),
        )
        .await;

        // Every one of the three files (INCLUDING both duplicate-hash entries) must
        // get a per-file bar reaching its own size, each carrying bytes_total ==
        // that file's size — the two duplicates each attributed to their OWN
        // rel_path by index, never collapsed onto one.
        for (rel, size) in &expected {
            let peak = evs
                .iter()
                .filter_map(|e| match e {
                    TransportEvent::ServeFileProgress {
                        package_id,
                        file,
                        bytes_done,
                        bytes_total,
                    } if *package_id == announce.package_id && file == rel => {
                        assert_eq!(
                            *bytes_total, *size,
                            "bytes_total for {rel} must equal its size"
                        );
                        Some(*bytes_done)
                    }
                    _ => None,
                })
                .max();
            assert_eq!(
                peak,
                Some(*size),
                "file {rel} must get a per-file ServeFileProgress bar ending at its size ({size}); \
                 got events {evs:?}"
            );
        }

        s.shutdown().await;
        r.shutdown().await;
    }

    // Task 2.1 (resume): `ServeComplete` fires on the terminal of the completing
    // payload request EVEN WHEN the resumed fetch requests fewer bytes than the
    // announced `byte_size` — precisely why the design forbids any byte-threshold
    // completion guard. We model a killed-then-resumed transfer deterministically:
    // pre-seed the receiver's store with the LARGER frame's content
    // (content-addressed, so the downloader treats it as already present), then a
    // full fetch pulls only the still-missing smaller frame. That completing
    // payload-carrying request must still route a ServeComplete.
    #[tokio::test]
    async fn resume_fires_serve_complete_despite_partial_bytes() {
        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r = bind_disabled(dr.path()).await;

        let out = s.handle(Role::Out);
        let recv = r.handle(Role::Recv);
        let s_info = out.start().await.unwrap();
        let r_info = recv.start().await.unwrap();
        pair(&s, &s_info, &r, &r_info);

        // frame_a = 4 KiB, frame_b = 64 KiB; announced byte_size = 68 KiB.
        let (pkg_dir, announce) = build_two_frame_package(ds.path(), 4 * 1024, 64 * 1024);
        out.serve(&announce, &pkg_dir, None).await.unwrap();

        // Pre-seed the receiver store with the 64 KiB frame so the fetch resumes,
        // pulling only the 4 KiB frame — far fewer bytes than the 68 KiB byte_size.
        // A byte-threshold completion guard would wrongly withhold ServeComplete
        // here; there must be none.
        let seeded = r
            .store()
            .blobs()
            .add_path(pkg_dir.join("frame_b.fits"))
            .temp_tag()
            .await
            .expect("pre-seed frame_b into the receiver store");

        let mut out_events = out.events().await;
        let mut r_ev = recv.events().await;
        out.announce(r_info.node_id, &announce, "", "", &[], PackageLayout::Batch)
            .await
            .unwrap();
        let wire = match recv_next(&mut r_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce,
            other => panic!("expected AnnounceReceived, got {other:?}"),
        };

        let fdest = tempdir().unwrap();
        recv.fetch(
            s_info.node_id,
            &wire,
            fdest.path(),
            crate::sharing::noop_fetch_sink(),
        )
        .await
        .unwrap();
        drop(seeded); // the pre-seed temp tag is no longer needed

        let events = collect_until_quiet(
            &mut out_events,
            Duration::from_millis(1500),
            Duration::from_secs(10),
        )
        .await;
        assert!(
            events.iter().any(|e| matches!(
                e,
                TransportEvent::ServeComplete { package_id } if *package_id == announce.package_id
            )),
            "a resumed fetch requesting fewer bytes than byte_size must still fire ServeComplete, got {events:?}"
        );

        s.shutdown().await;
        r.shutdown().await;
    }

    // -----------------------------------------------------------------------
    // Task 2: event demux ((peer, package) ack claims) + pooled control conn.
    // Every test runs two/three real SharedIrohNodes in-process with the relay
    // disabled, paired over localhost direct addresses — CI-safe, no network.
    // -----------------------------------------------------------------------

    use crate::sharing::types::ReceiptOutcome;
    use std::time::Duration;
    use tokio::sync::mpsc::Receiver;

    /// A minimal announce carrying just the ack-correlation `package_id` (the
    /// demux tests never fetch, so the placeholder root_hash is fine).
    fn mk_announce(id: &str) -> PackageAnnounce {
        PackageAnnounce {
            package_id: PackageId(id.to_string()),
            root_hash: "placeholder".to_string(),
            byte_size: 0,
            frame_count: 0,
        }
    }

    fn mk_receipts() -> Vec<FrameReceipt> {
        vec![FrameReceipt {
            frame_uuid: "u".to_string(),
            xxh3: "h".to_string(),
            outcome: ReceiptOutcome::Ingested,
        }]
    }

    async fn recv_next(rx: &mut Receiver<TransportEvent>) -> TransportEvent {
        tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("event channel stalled")
            .expect("event channel closed unexpectedly")
    }

    async fn wait_until<F: FnMut() -> bool>(mut pred: F, timeout: Duration) {
        let start = std::time::Instant::now();
        loop {
            if pred() {
                return;
            }
            if start.elapsed() >= timeout {
                panic!("wait_until timed out after {timeout:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_until_claims(node: &Arc<SharedIrohNode>, want: usize) {
        wait_until(|| node.active_claims() == want, Duration::from_secs(5)).await;
    }

    async fn bind_disabled(dir: &Path) -> Arc<SharedIrohNode> {
        SharedIrohNode::bind(dir, RelayMode::Disabled)
            .await
            .expect("bind relay-disabled node")
    }

    /// Mutually register two paired nodes' addresses (each learns the other's).
    fn pair(
        a: &Arc<SharedIrohNode>,
        a_info: &StartInfo,
        b: &Arc<SharedIrohNode>,
        b_info: &StartInfo,
    ) {
        a.add_peer_ticket(&b_info.pairing_ticket)
            .expect("a pairs b");
        b.add_peer_ticket(&a_info.pairing_ticket)
            .expect("b pairs a");
    }

    // (a) Two Out handles announce to DIFFERENT peers concurrently; each ack
    //     routes to its own announcing handle — no cross-talk.
    #[tokio::test]
    async fn concurrent_acks_route_to_the_right_out_handle() {
        let ds = tempdir().unwrap();
        let d1 = tempdir().unwrap();
        let d2 = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r1 = bind_disabled(d1.path()).await;
        let r2 = bind_disabled(d2.path()).await;

        let out_a = s.handle(Role::Out);
        let out_b = s.handle(Role::Out);
        let recv1 = r1.handle(Role::Recv);
        let recv2 = r2.handle(Role::Recv);

        let s_info = out_a.start().await.unwrap();
        let r1_info = recv1.start().await.unwrap();
        let r2_info = recv2.start().await.unwrap();
        pair(&s, &s_info, &r1, &r1_info);
        pair(&s, &s_info, &r2, &r2_info);

        let mut ack_a = out_a.events().await;
        let mut ack_b = out_b.events().await;
        let mut r1_ev = recv1.events().await;
        let mut r2_ev = recv2.events().await;

        let p1 = mk_announce("pkg-a");
        let p2 = mk_announce("pkg-b");

        // Announce to the two distinct peers (delivery completes when each
        // receiver's Recv consumer receives the announce).
        out_a
            .announce(r1_info.node_id, &p1, "", "", &[], PackageLayout::Batch)
            .await
            .unwrap();
        out_b
            .announce(r2_info.node_id, &p2, "", "", &[], PackageLayout::Batch)
            .await
            .unwrap();

        // Each receiver observes its own announce, then acks back to the sender.
        let pid1 = match recv_next(&mut r1_ev).await {
            TransportEvent::AnnounceReceived { from, announce, .. } => {
                assert_eq!(from, s_info.node_id);
                announce.package_id
            }
            other => panic!("expected AnnounceReceived on r1, got {other:?}"),
        };
        let pid2 = match recv_next(&mut r2_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce.package_id,
            other => panic!("expected AnnounceReceived on r2, got {other:?}"),
        };
        recv1
            .ack(s_info.node_id, &pid1, mk_receipts())
            .await
            .unwrap();
        recv2
            .ack(s_info.node_id, &pid2, mk_receipts())
            .await
            .unwrap();

        // The acks route to the correct Out handle — no cross-delivery.
        match recv_next(&mut ack_a).await {
            TransportEvent::AckReceived {
                from, package_id, ..
            } => {
                assert_eq!(from, r1_info.node_id, "out_a's ack must come from r1");
                assert_eq!(package_id, p1.package_id);
            }
            other => panic!("expected AckReceived on out_a, got {other:?}"),
        }
        match recv_next(&mut ack_b).await {
            TransportEvent::AckReceived {
                from, package_id, ..
            } => {
                assert_eq!(from, r2_info.node_id, "out_b's ack must come from r2");
                assert_eq!(package_id, p2.package_id);
            }
            other => panic!("expected AckReceived on out_b, got {other:?}"),
        }
        assert!(
            ack_a.try_recv().is_err(),
            "out_a must not receive out_b's ack"
        );
        assert!(
            ack_b.try_recv().is_err(),
            "out_b must not receive out_a's ack"
        );
        // Both claims consumed on their acks.
        wait_until_claims(&s, 0).await;

        s.shutdown().await;
        r1.shutdown().await;
        r2.shutdown().await;
    }

    // (b) The SAME package id announced to two peers (Perseus multi-dest shape):
    //     the two `(peer, package)` claims are distinct, and each ack completes
    //     ONLY its own claim.
    #[tokio::test]
    async fn same_package_id_two_peers_each_ack_completes_only_its_claim() {
        let ds = tempdir().unwrap();
        let d1 = tempdir().unwrap();
        let d2 = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r1 = bind_disabled(d1.path()).await;
        let r2 = bind_disabled(d2.path()).await;

        let out = s.handle(Role::Out); // one sender, two destinations
        let recv1 = r1.handle(Role::Recv);
        let recv2 = r2.handle(Role::Recv);

        let s_info = out.start().await.unwrap();
        let r1_info = recv1.start().await.unwrap();
        let r2_info = recv2.start().await.unwrap();
        pair(&s, &s_info, &r1, &r1_info);
        pair(&s, &s_info, &r2, &r2_info);

        let mut acks = out.events().await;
        let mut r1_ev = recv1.events().await;
        let mut r2_ev = recv2.events().await;

        // ONE package id, announced to two peers.
        let pkg = mk_announce("shared-pkg");
        out.announce(r1_info.node_id, &pkg, "", "", &[], PackageLayout::Batch)
            .await
            .unwrap();
        out.announce(r2_info.node_id, &pkg, "", "", &[], PackageLayout::Batch)
            .await
            .unwrap();
        assert_eq!(s.active_claims(), 2, "two distinct (peer, package) claims");

        let pid1 = match recv_next(&mut r1_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce.package_id,
            other => panic!("expected AnnounceReceived on r1, got {other:?}"),
        };
        let pid2 = match recv_next(&mut r2_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce.package_id,
            other => panic!("expected AnnounceReceived on r2, got {other:?}"),
        };

        // r1 acks: only the (r1, pkg) claim is consumed.
        recv1
            .ack(s_info.node_id, &pid1, mk_receipts())
            .await
            .unwrap();
        match recv_next(&mut acks).await {
            TransportEvent::AckReceived { from, .. } => assert_eq!(from, r1_info.node_id),
            other => panic!("expected r1 ack, got {other:?}"),
        }
        wait_until_claims(&s, 1).await;

        // r2 acks: the (r2, pkg) claim is consumed too.
        recv2
            .ack(s_info.node_id, &pid2, mk_receipts())
            .await
            .unwrap();
        match recv_next(&mut acks).await {
            TransportEvent::AckReceived { from, .. } => assert_eq!(from, r2_info.node_id),
            other => panic!("expected r2 ack, got {other:?}"),
        }
        wait_until_claims(&s, 0).await;

        s.shutdown().await;
        r1.shutdown().await;
        r2.shutdown().await;
    }

    // (c) An announce arriving with NO Recv consumer registered is an orphan:
    //     the receiver logs the orphan warn and withholds the delivery ack, so
    //     the sender never gets a silent success (it errors/times out → retry).
    #[tokio::test]
    async fn orphan_announce_warns_and_withholds_delivery_ack() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let captured: Arc<std::sync::Mutex<Vec<CapturedEvent>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let _guard = tracing_subscriber::registry()
            .with(CaptureLayer {
                events: captured.clone(),
            })
            .set_default();

        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r = bind_disabled(dr.path()).await;

        let out = s.handle(Role::Out);
        // A Recv handle exists but NEVER takes events() → no Recv consumer.
        let recv = r.handle(Role::Recv);

        let s_info = out.start().await.unwrap();
        let r_info = recv.start().await.unwrap();
        pair(&s, &s_info, &r, &r_info);

        // Take the Out handle's events so its ack claim is registered — the
        // orphan under test is the RECEIVER-side announce with no Recv consumer.
        let _ack = out.events().await;

        let pkg = mk_announce("orphan-pkg");
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            out.announce(r_info.node_id, &pkg, "", "", &[], PackageLayout::Batch),
        )
        .await;
        assert!(
            !matches!(outcome, Ok(Ok(()))),
            "an orphan announce must not receive a silent delivery ack, got {outcome:?}"
        );

        // The receiver logged the orphan warn naming the event kind.
        wait_until(
            || {
                captured.lock().unwrap().iter().any(|e| {
                    e.message == "inbound event with no consumer"
                        && e.fields.get("kind").map(String::as_str) == Some("announce")
                })
            },
            Duration::from_secs(5),
        )
        .await;

        drop(_guard);
        s.shutdown().await;
        r.shutdown().await;
    }

    // (d) Two sequential control sends to one peer reuse a single pooled
    //     connection (only one real dial).
    #[tokio::test]
    async fn sequential_control_sends_reuse_one_pooled_connection() {
        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r = bind_disabled(dr.path()).await;

        let out = s.handle(Role::Out);
        let recv = r.handle(Role::Recv);

        let s_info = out.start().await.unwrap();
        let r_info = recv.start().await.unwrap();
        pair(&s, &s_info, &r, &r_info);

        let _ack = out.events().await;
        let mut r_ev = recv.events().await; // Recv consumer → announces deliver + ack
        let pkg = mk_announce("pool-pkg");

        out.announce(r_info.node_id, &pkg, "", "", &[], PackageLayout::Batch)
            .await
            .unwrap();
        out.announce(r_info.node_id, &pkg, "", "", &[], PackageLayout::Batch)
            .await
            .unwrap();

        // Both delivered to the receiver's single Recv consumer.
        let _ = recv_next(&mut r_ev).await;
        let _ = recv_next(&mut r_ev).await;

        assert_eq!(
            s.control_pool_dials(),
            1,
            "two sequential control sends to one peer must reuse a single pooled connection"
        );

        s.shutdown().await;
        r.shutdown().await;
    }

    // (e) The Task-9 holder probe classifies a refusing holder as `refused` (the
    //     connect handshake succeeds, then the gate closes the connection) and an
    //     address-less holder as `offline`. Two real loopback nodes, relay disabled.
    #[tokio::test]
    async fn probe_holder_classifies_refused_and_offline() {
        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let client = bind_disabled(ds.path()).await;
        let server = bind_disabled(dr.path()).await;

        let c = client.handle(Role::Collab);
        let s = server.handle(Role::Recv);
        let c_info = c.start().await.unwrap();
        let s_info = s.start().await.unwrap();
        pair(&client, &c_info, &server, &s_info);

        // The server refuses every inbound connection at the gate.
        server.set_connect_gate(Arc::new(|_: &NodeId| false));

        // Refused: the connect handshake succeeds, then the gate closes it.
        let refused = client
            .probe_holder(s_info.node_id, false, Duration::from_secs(5))
            .await;
        assert_eq!(
            refused,
            Err(ProbeClass::Refused),
            "a gated holder must classify as refused"
        );

        // Offline: a node id we never learned an address for is undialable.
        let offline = client
            .probe_holder([42u8; 32], false, Duration::from_secs(2))
            .await;
        assert_eq!(
            offline,
            Err(ProbeClass::Offline),
            "an address-less holder must classify as offline"
        );

        client.shutdown().await;
        server.shutdown().await;
    }

    // (f) The pure connect-error classifier (Task 9): a relay-hinted timeout is
    //     `relay_unreachable`; the same timeout with no relay hint is `offline`; an
    //     application close is `refused`; a no-addressing error is `offline`.
    #[test]
    fn classify_connect_err_maps_each_class() {
        assert_eq!(
            classify_connect_err("connection timed out", true),
            ProbeClass::RelayUnreachable
        );
        assert_eq!(
            classify_connect_err("connection timed out", false),
            ProbeClass::Offline
        );
        assert_eq!(
            classify_connect_err("closed by peer: unauthorized", false),
            ProbeClass::Refused
        );
        assert_eq!(
            classify_connect_err("no addressing information available", true),
            ProbeClass::Offline
        );
    }

    // -----------------------------------------------------------------------
    // Task 3.5: relay-map refresh + live hot-swap (was: idle node rebuild).
    // -----------------------------------------------------------------------

    // (0) The pure set diff that drives a hot-swap: `insert_relay` for urls newly
    //     present, `remove_relay` for urls gone. Order-insensitive.
    #[test]
    fn relay_set_diff_is_the_symmetric_difference() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();

        // Overlap: one added, one removed, the rest unchanged.
        let (added, removed) = relay_set_diff(&s(&["a", "b", "c"]), &s(&["b", "c", "d"]));
        assert_eq!(added, s(&["d"]), "d is newly present");
        assert_eq!(removed, s(&["a"]), "a is gone");

        // Same set, different order → no change.
        let (added, removed) = relay_set_diff(&s(&["a", "b"]), &s(&["b", "a"]));
        assert!(
            added.is_empty() && removed.is_empty(),
            "reordering is not a change"
        );

        // Empty new → everything removed; empty old → everything added.
        assert_eq!(
            relay_set_diff(&s(&["a", "b"]), &s(&[])),
            (s(&[]), s(&["a", "b"]))
        );
        assert_eq!(relay_set_diff(&s(&[]), &s(&["a"])), (s(&["a"]), s(&[])));
    }

    // (a) A relay-map hot-swap on the LIVE endpoint preserves identity, the shared
    //     store, AND the in-flight connection: it re-homes the SAME endpoint via
    //     `insert_relay`/`remove_relay` (no rebuild), so a pooled control
    //     connection opened before the swap is REUSED after it (`control_pool_dials`
    //     stays flat — the whole point of Task 3.5) with no re-pairing, and a full
    //     announce→ack round trip still completes. `relay_urls()` reflects the new
    //     set. Relay is a fake-but-valid url (no server needed: `insert_relay` only
    //     updates the map + re-STUNs in the background, never blocks on a dial).
    #[tokio::test]
    async fn relay_hot_swap_preserves_identity_store_connection_and_handles() {
        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let s = bind_disabled(ds.path()).await;
        let r = bind_disabled(dr.path()).await;

        let out = s.handle(Role::Out);
        let recv = r.handle(Role::Recv);
        let s_info = out.start().await.unwrap();
        let r_info = recv.start().await.unwrap();
        pair(&s, &s_info, &r, &r_info);

        let id_before = s.node_id();
        assert_eq!(
            *s.endpoint_addr().id.as_bytes(),
            id_before,
            "addr id == node id pre-swap"
        );

        // Seed a tag on the shared store to prove the store is untouched by a swap.
        let tt = s
            .store()
            .blobs()
            .add_bytes(b"survive".to_vec())
            .temp_tag()
            .await
            .unwrap();
        s.store()
            .tags()
            .set("out/pkg/keep", tt.hash_and_format())
            .await
            .unwrap();
        drop(tt);

        // Open a live pooled control connection: one announce→ack round trip.
        let mut acks = out.events().await;
        let mut r_ev = recv.events().await;
        let pkg1 = mk_announce("pre-swap");
        out.announce(r_info.node_id, &pkg1, "", "", &[], PackageLayout::Batch)
            .await
            .unwrap();
        let pid1 = match recv_next(&mut r_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce.package_id,
            other => panic!("expected AnnounceReceived pre-swap, got {other:?}"),
        };
        recv.ack(s_info.node_id, &pid1, mk_receipts())
            .await
            .unwrap();
        match recv_next(&mut acks).await {
            TransportEvent::AckReceived { package_id, .. } => {
                assert_eq!(package_id, pkg1.package_id)
            }
            other => panic!("expected AckReceived pre-swap, got {other:?}"),
        }
        assert_eq!(
            s.control_pool_dials(),
            1,
            "one pooled dial established pre-swap"
        );

        // Hot-swap the sender's relay map onto a fresh (fake-but-valid) relay set.
        let new_map = iroh::RelayMap::try_from_iter(["https://127.0.0.1:9"]).unwrap();
        let want_urls: Vec<String> = new_map
            .urls::<Vec<_>>()
            .iter()
            .map(|u| u.to_string())
            .collect();
        let changed = s.apply_relay_change(RelayMode::Custom(new_map)).await;
        assert!(changed, "inserting a relay onto the empty set is a change");

        // Identity + store + reported set all reflect the swap without a rebuild.
        assert_eq!(
            s.node_id(),
            id_before,
            "node id stable across a relay hot-swap"
        );
        assert!(
            tag_present(s.store(), "out/pkg/keep").await,
            "the shared store (and its tag) must survive a relay hot-swap"
        );
        assert_eq!(
            *s.endpoint_addr().id.as_bytes(),
            id_before,
            "endpoint_addr id stable across a hot-swap (same endpoint, re-homed in place)"
        );
        assert_eq!(
            s.relay_urls(),
            want_urls,
            "relay_urls() must reflect the hot-swapped set"
        );

        // The pre-swap connection was NOT dropped: a second announce→ack reuses the
        // SAME pooled connection (dials still 1). No re-pairing — the endpoint is
        // re-homed in place, its direct addr unchanged. A rebuild would have reset
        // the pool and forced a re-dial here.
        let pkg2 = mk_announce("post-swap");
        out.announce(r_info.node_id, &pkg2, "", "", &[], PackageLayout::Batch)
            .await
            .unwrap();
        let pid2 = match recv_next(&mut r_ev).await {
            TransportEvent::AnnounceReceived { announce, .. } => announce.package_id,
            other => panic!("expected AnnounceReceived post-swap, got {other:?}"),
        };
        recv.ack(s_info.node_id, &pid2, mk_receipts())
            .await
            .unwrap();
        match recv_next(&mut acks).await {
            TransportEvent::AckReceived { package_id, .. } => {
                assert_eq!(package_id, pkg2.package_id)
            }
            other => panic!("expected AckReceived post-swap, got {other:?}"),
        }
        assert_eq!(
            s.control_pool_dials(),
            1,
            "the live pooled connection must survive the relay hot-swap (no re-dial)"
        );

        s.shutdown().await;
        r.shutdown().await;
    }

    // (b) `apply_relay_change` is a no-op on an unchanged set (returns false, no
    //     log), and on a real change logs the hot-swap line carrying added/removed
    //     counts. Drives add → same → remove and asserts each verdict + the log.
    #[tokio::test]
    async fn relay_hot_swap_noop_when_unchanged_else_logs_diff() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let captured: Arc<std::sync::Mutex<Vec<CapturedEvent>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let _guard = tracing_subscriber::registry()
            .with(CaptureLayer {
                events: captured.clone(),
            })
            .set_default();

        let dir = tempdir().unwrap();
        let node = bind_disabled(dir.path()).await;
        let id = node.node_id();

        // Baseline is the empty relay-disabled set → re-applying Disabled is a no-op.
        assert!(
            !node.apply_relay_change(RelayMode::Disabled).await,
            "re-applying the unchanged (empty) set must be a no-op"
        );

        // Add one relay → a change (added=1, removed=0).
        let map = iroh::RelayMap::try_from_iter(["https://127.0.0.1:9"]).unwrap();
        assert!(
            node.apply_relay_change(RelayMode::Custom(map.clone()))
                .await,
            "adding a relay is a change"
        );
        assert_eq!(node.relay_urls().len(), 1, "one relay now configured");

        // Re-apply the SAME set → no-op.
        assert!(
            !node.apply_relay_change(RelayMode::Custom(map)).await,
            "re-applying the same set must be a no-op"
        );

        // Remove it (back to Disabled) → a change (added=0, removed=1).
        assert!(
            node.apply_relay_change(RelayMode::Disabled).await,
            "removing the last relay is a change"
        );
        assert!(node.relay_urls().is_empty(), "no relays after removal");
        assert_eq!(node.node_id(), id, "node id stable across every hot-swap");

        // Exactly two hot-swap log lines (the add and the remove), carrying counts.
        let events = captured.lock().unwrap();
        let swaps: Vec<&CapturedEvent> = events
            .iter()
            .filter(|e| e.message == "relay map hot-swapped on live endpoint")
            .collect();
        assert_eq!(
            swaps.len(),
            2,
            "one log per applied change; captured: {:?}",
            events.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        );
        assert_eq!(swaps[0].fields.get("added").map(String::as_str), Some("1"));
        assert_eq!(
            swaps[0].fields.get("removed").map(String::as_str),
            Some("0")
        );
        assert_eq!(swaps[1].fields.get("added").map(String::as_str), Some("0"));
        assert_eq!(
            swaps[1].fields.get("removed").map(String::as_str),
            Some("1")
        );
        drop(events);

        node.shutdown().await;
    }

    // (c) S4 survives a relay hot-swap: the router/gate are never rebuilt, so an
    //     installed deny-SPECIFIC-peer gate keeps its verdict after the swap —
    //     asserted BOTH ways (denied peer still refused with zero control dispatch;
    //     allowed peer still admitted). No re-pairing: the endpoint is re-homed in
    //     place, so the senders' addresses stay valid.
    #[tokio::test]
    async fn connect_gate_survives_relay_hot_swap() {
        let dr = tempdir().unwrap();
        let dd = tempdir().unwrap();
        let da = tempdir().unwrap();
        let r = bind_disabled(dr.path()).await; // gated receiver
        let denied = bind_disabled(dd.path()).await;
        let allowed = bind_disabled(da.path()).await;

        let recv = r.handle(Role::Recv);
        let out_denied = denied.handle(Role::Out);
        let out_allowed = allowed.handle(Role::Out);

        let r_info = recv.start().await.unwrap();
        let denied_info = out_denied.start().await.unwrap();
        let allowed_info = out_allowed.start().await.unwrap();
        pair(&r, &r_info, &denied, &denied_info);
        pair(&r, &r_info, &allowed, &allowed_info);

        // Deny the denied sender's node id; admit everyone else.
        let denied_id = denied.node_id();
        r.set_connect_gate(Arc::new(move |from: &NodeId| *from != denied_id));

        // Hot-swap the receiver's relay map onto a fresh set (no rebuild — same
        // router, same gate, same endpoint re-homed in place).
        let new_map = iroh::RelayMap::try_from_iter(["https://127.0.0.1:9"]).unwrap();
        assert!(
            r.apply_relay_change(RelayMode::Custom(new_map)).await,
            "the receiver's relay set changed"
        );

        let mut r_events = recv.events().await;

        // (1) The denied peer is STILL refused after the hot-swap.
        let pkg_denied = mk_announce("post-swap-denied");
        let outcome = tokio::time::timeout(
            Duration::from_secs(15),
            out_denied.announce(
                r_info.node_id,
                &pkg_denied,
                "",
                "",
                &[],
                PackageLayout::Batch,
            ),
        )
        .await;
        match outcome {
            Ok(Ok(())) => {
                panic!("a denied peer's announce must not succeed after a relay hot-swap")
            }
            Ok(Err(_)) | Err(_) => {} // expected: refused → no delivery ack, ever
        }
        // Zero control dispatch from the denied peer (the gate closed the
        // connection before any `Msg` was decoded).
        let never_arrived = tokio::time::timeout(Duration::from_millis(300), r_events.recv()).await;
        assert!(
            never_arrived.is_err(),
            "the denied peer must produce zero control dispatch post-swap"
        );

        // (2) An allowed peer is STILL admitted after the hot-swap.
        let pkg_allowed = mk_announce("post-swap-allowed");
        out_allowed
            .announce(
                r_info.node_id,
                &pkg_allowed,
                "",
                "",
                &[],
                PackageLayout::Batch,
            )
            .await
            .expect("an allowed peer must still be admitted after the relay hot-swap");
        match recv_next(&mut r_events).await {
            TransportEvent::AnnounceReceived { from, .. } => assert_eq!(
                from, allowed_info.node_id,
                "the admitted announce must come from the allowed peer"
            ),
            other => panic!("expected AnnounceReceived from the allowed peer, got {other:?}"),
        }

        r.shutdown().await;
        denied.shutdown().await;
        allowed.shutdown().await;
    }

    // (d) The refresh loop's silent branch: a resolver that fails to resolve
    //     (returns `None`, a hub blip) keeps the current relay map AND surfaces a
    //     `warn!` — a silent keep would hide a hub unreachable for hours.
    #[tokio::test]
    async fn relay_refresh_resolver_failure_warns_and_keeps_map() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let captured: Arc<std::sync::Mutex<Vec<CapturedEvent>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let _guard = tracing_subscriber::registry()
            .with(CaptureLayer {
                events: captured.clone(),
            })
            .set_default();

        let dir = tempdir().unwrap();
        let node = bind_disabled(dir.path()).await;
        let before = node.relay_urls();

        // A resolver that always fails (hub blip): keep the map, but not silently.
        let resolver: RelayResolver = Arc::new(|| Box::pin(async { None }));
        node.start_relay_refresh(resolver);
        node.request_relay_refresh();

        wait_until(
            || {
                captured
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| e.message == "relay map refresh failed; keeping current map")
            },
            Duration::from_secs(5),
        )
        .await;

        assert_eq!(
            node.relay_urls(),
            before,
            "a failed refresh must keep the current relay map"
        );

        drop(_guard);
        node.shutdown().await;
    }

    // A minimal thread-local tracing capture (mirrors the iroh tests.rs layer),
    // scoped to `athenaeum_core::sharing` so the orphan warn can be asserted
    // without touching the global JSONL subscriber.
    #[derive(Clone, Default)]
    struct CapturedEvent {
        message: String,
        fields: std::collections::HashMap<String, String>,
    }

    #[derive(Default)]
    struct FieldCollector {
        message: String,
        fields: std::collections::HashMap<String, String>,
    }

    impl tracing::field::Visit for FieldCollector {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            let rendered = format!("{value:?}");
            if field.name() == "message" {
                self.message = rendered;
            } else {
                self.fields.insert(field.name().to_string(), rendered);
            }
        }
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "message" {
                self.message = value.to_string();
            } else {
                self.fields
                    .insert(field.name().to_string(), value.to_string());
            }
        }
    }

    #[derive(Clone)]
    struct CaptureLayer {
        events: Arc<std::sync::Mutex<Vec<CapturedEvent>>>,
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureLayer {
        fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
            Some(tracing::level_filters::LevelFilter::INFO)
        }

        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if !event
                .metadata()
                .target()
                .starts_with("athenaeum_core::sharing")
            {
                return;
            }
            let mut collector = FieldCollector::default();
            event.record(&mut collector);
            self.events.lock().unwrap().push(CapturedEvent {
                message: collector.message,
                fields: collector.fields,
            });
        }
    }
}
