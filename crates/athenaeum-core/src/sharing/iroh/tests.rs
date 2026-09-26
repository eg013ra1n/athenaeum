//! In-process acceptance tests for the iroh transport (task A5).
//!
//! Every test runs two real iroh endpoints in the same process with the relay
//! **disabled** and connects over localhost direct addresses — no external
//! network, so they are CI-safe. Endpoints pair by exchanging their
//! [`StartInfo::pairing_ticket`] (an `EndpointTicket`), mirroring the real
//! out-of-band pairing flow.
//!
//! The three named tests from the brief:
//! - [`iroh_roundtrip_two_endpoints_localhost`] — the loopback round-trip's
//!   assertions (announce → fetch → ack), over iroh.
//! - [`iroh_resume_after_endpoint_restart`] — interrupt a fetch, drop + recreate
//!   the receiving endpoint over the same persistent blob store; re-fetch
//!   completes and hash-verifies. Proves restart-then-complete, not partial-range
//!   resume specifically — see the test's inline comment.
//! - [`engine_suite_over_iroh`] (+ [`engine_dup_ack_confirms_once_over_iroh`]) —
//!   the A4 engine's happy-path and duplicate-ack scenarios driven over iroh.
//!
//! Plus [`fetch_rejects_traversal_entry_names`] — a peer-supplied collection
//! entry name must never escape `dest_dir` (the fetch-side counterpart of the
//! A3 write-side `rel_path` guard).
//!
//! And the collab-exchange slice-4 connect gate:
//! [`connect_gate_refuses_control_dispatch_and_blocks_announce`] /
//! [`connect_gate_permits_when_predicate_allows`] /
//! [`connect_gate_refuses_blob_fetch`] — `IrohTransport::set_connect_gate`
//! actually gates BOTH protocol handlers over a real iroh connection: a
//! refusing gate blocks the control-channel announce (zero events dispatched)
//! and the blobs download (zero bytes land), while a permitting gate lets the
//! very same announce through unchanged.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::{EndpointId, RelayMode};
use iroh_blobs::api::Store;
use iroh_blobs::format::collection::Collection;
use iroh_blobs::protocol::{ChunkRanges, ChunkRangesExt, GetRequest};
use iroh_blobs::util::connection_pool::{ConnectionPool, Options as PoolOptions};
use iroh_blobs::Hash;
use tempfile::tempdir;
use tokio::sync::mpsc::Receiver;
use tokio::time::Instant;

use crate::package::{self, write_package, ManifestRecord, PayloadKind, MANIFEST_VERSION};
use crate::sharing::types::{
    FetchEvent, FrameReceipt, NodeId, PackageAnnounce, PackageId, PackageLayout, ReceiptOutcome,
    TransportEvent,
};
use crate::sharing::{
    noop_fetch_sink, FetchSink, ProviderEvent, ProviderTelemetrySink, SharingTransport,
};
use crate::sync::{HistoryQuery, OutboundState, StandaloneSyncStore, SyncEngine, SyncStore};

use super::assign::{AssignmentReport, ProviderStats, SwarmFetchMode};
use super::node::{NodeOptions, Role, SharedIrohNode};
use super::{random_secret, BlobStore, IrohTransport};

/// A [`FetchSink`] that appends every event into a shared vec, plus the vec so a
/// test can inspect the collected series after the fetch — the iroh-side
/// counterpart of `sharing::tests::recording_sink`, pinning the REAL emission
/// path (per-file observer tasks + the aggregate download stream), not just the
/// loopback mock's synthetic one.
fn recording_sink() -> (FetchSink, Arc<Mutex<Vec<FetchEvent>>>) {
    let events: Arc<Mutex<Vec<FetchEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_events = Arc::clone(&events);
    let sink: FetchSink = Arc::new(move |ev| {
        sink_events.lock().expect("sink mutex poisoned").push(ev);
    });
    (sink, events)
}

/// Generous ceiling for a single event / transfer over a freshly-established
/// QUIC connection (bind + handshake + transfer), well above the localhost norm.
const IROH_WAIT: Duration = Duration::from_secs(60);

/// Build a fresh in-memory transport with the relay disabled (direct localhost).
/// No dedup responder — most tests don't exercise the handshake.
async fn mem_transport() -> IrohTransport {
    IrohTransport::new(
        random_secret(),
        RelayMode::Disabled,
        BlobStore::Memory,
        None,
    )
    .await
    .expect("build iroh transport")
}

/// Like [`mem_transport`] but wires a dedup responder into the control channel,
/// so this endpoint answers a peer's `negotiate_want` from `responder`.
async fn mem_transport_with_responder(
    responder: Arc<dyn crate::sync::DedupResponder>,
) -> IrohTransport {
    IrohTransport::new(
        random_secret(),
        RelayMode::Disabled,
        BlobStore::Memory,
        Some(responder),
    )
    .await
    .expect("build iroh transport with responder")
}

/// Bring two endpoints online and pair them (each learns the other's address).
async fn start_and_pair(
    a: &IrohTransport,
    b: &IrohTransport,
) -> (
    crate::sharing::types::StartInfo,
    crate::sharing::types::StartInfo,
) {
    let a_info = a.start().await.expect("start a");
    let b_info = b.start().await.expect("start b");
    a.add_peer_ticket(&b_info.pairing_ticket)
        .expect("a pairs b");
    b.add_peer_ticket(&a_info.pairing_ticket)
        .expect("b pairs a");
    (a_info, b_info)
}

/// Write a one-frame package (payload + manifest) and return `(pkg_dir, announce)`.
/// `announce.root_hash` is the xxh3 placeholder — the transport swaps in the iroh
/// collection hash at `serve`/`announce` time.
fn build_package(
    src_root: &Path,
    frame_uuid: &str,
    filename: &str,
    object: &str,
    size: usize,
) -> (PathBuf, PackageAnnounce) {
    std::fs::create_dir_all(src_root).unwrap();
    let payload = src_root.join(filename);
    let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    std::fs::write(&payload, &bytes).unwrap();

    let byte_size = std::fs::metadata(&payload).unwrap().len();
    let xxh3 = package::xxh3_full_file(&payload).unwrap();
    let record = ManifestRecord {
        v: MANIFEST_VERSION,
        frame_uuid: frame_uuid.to_string(),
        origin_catalog_uuid: "catalog-uuid".to_string(),
        origin_device: "origin-device".to_string(),
        payload_kind: PayloadKind::RawFrame,
        rel_path: filename.to_string(),
        byte_size,
        xxh3,
        frame_meta: serde_json::json!({ "object": object }),
        analysis: None,
        app_version: "test".to_string(),
        project: None,
    };

    let pkg_dir = src_root.parent().unwrap().join(format!("pkg-{frame_uuid}"));
    let announce = write_package(&pkg_dir, vec![(payload, record)]).unwrap();
    (pkg_dir, announce)
}

fn xxh3_of(path: &Path) -> String {
    package::xxh3_full_file(path).unwrap()
}

async fn recv_next(rx: &mut Receiver<TransportEvent>) -> TransportEvent {
    tokio::time::timeout(IROH_WAIT, rx.recv())
        .await
        .expect("event channel stalled")
        .expect("event channel closed unexpectedly")
}

async fn wait_until<F: FnMut() -> bool>(mut pred: F, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("wait_until timed out after {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn state_of(store: &StandaloneSyncStore, id: i64) -> Option<OutboundState> {
    store.get_outbound(id).unwrap().map(|r| r.state)
}

// ---------------------------------------------------------------------------
// 1. Round-trip: announce → fetch → ack (loopback parity).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn iroh_roundtrip_two_endpoints_localhost() {
    let provider = mem_transport().await;
    let receiver = mem_transport().await;
    let (provider_info, receiver_info) = start_and_pair(&provider, &receiver).await;

    let mut provider_events = provider.events().await;
    let mut receiver_events = receiver.events().await;

    let tmp = tempdir().unwrap();
    let (pkg_dir, announce) = build_package(
        &tmp.path().join("src"),
        "uuid-1",
        "frame1.fits",
        "M42",
        128 * 1024,
    );

    provider
        .serve(&announce, &pkg_dir, None, None)
        .await
        .unwrap();
    provider
        .announce(
            receiver_info.node_id,
            &announce,
            "",
            "",
            &[],
            PackageLayout::Batch,
        )
        .await
        .unwrap();

    // Receiver observes the announce — now carrying the iroh collection hash.
    let wire = match recv_next(&mut receiver_events).await {
        TransportEvent::AnnounceReceived { from, announce, .. } => {
            assert_eq!(from, provider_info.node_id);
            announce
        }
        other => panic!("expected AnnounceReceived, got {other:?}"),
    };
    assert_eq!(wire.package_id, announce.package_id, "package_id preserved");
    assert_ne!(
        wire.root_hash, announce.root_hash,
        "announce should carry the iroh collection hash, not the xxh3 placeholder"
    );

    // Receiver fetches into its own dir and verifies content + manifest. A
    // recording sink pins the REAL iroh emission path (per-file observer tasks
    // over `blobs().observe` + the aggregate download stream), not just the
    // loopback mock's synthetic one, and proves the observer-abort guard
    // (blobs.rs `AbortObserversOnDrop`) doesn't suppress the happy-path events.
    let (sink, events) = recording_sink();
    let dest = tempdir().unwrap();
    receiver
        .fetch(provider_info.node_id, &wire, dest.path(), sink)
        .await
        .unwrap();
    let fetched = dest.path().join("frame1.fits");
    assert!(fetched.exists(), "fetched payload missing");
    assert_eq!(
        xxh3_of(&pkg_dir.join("frame1.fits")),
        xxh3_of(&fetched),
        "content mismatch"
    );
    assert!(
        dest.path().join("manifest.ndjson").exists(),
        "manifest fetched as part of the collection"
    );

    // Per-file events (one series per collection entry — frame1.fits AND
    // manifest.ndjson) are non-decreasing and end complete; at least one Batch
    // event reaches the announce's byte_size.
    let events = events.lock().unwrap().clone();
    let mut per_file: std::collections::HashMap<String, Vec<(u64, u64)>> =
        std::collections::HashMap::new();
    for ev in &events {
        if let FetchEvent::File {
            name,
            bytes_done,
            bytes_total,
            ..
        } = ev
        {
            per_file
                .entry(name.clone())
                .or_default()
                .push((*bytes_done, *bytes_total));
        }
    }
    assert!(
        !per_file.is_empty(),
        "expected at least one File event over real iroh"
    );
    for (name, series) in &per_file {
        let mut last_done = 0u64;
        for (done, _total) in series {
            assert!(
                *done >= last_done,
                "File progress for {name} went backwards: {done} < {last_done}"
            );
            last_done = *done;
        }
        let (final_done, final_total) = *series.last().unwrap();
        assert_eq!(
            final_done, final_total,
            "File {name} must end complete (done == total)"
        );
    }
    let reached_total = events.iter().any(|ev| {
        matches!(
            ev,
            FetchEvent::Batch { bytes_done, bytes_total }
                if *bytes_done == wire.byte_size && *bytes_total == wire.byte_size
        )
    });
    assert!(
        reached_total,
        "expected a Batch event reaching byte_size={}, got {events:?}",
        wire.byte_size
    );

    // Receiver acks; provider observes the receipts.
    let receipts = vec![FrameReceipt {
        frame_uuid: "uuid-1".to_string(),
        xxh3: xxh3_of(&fetched),
        outcome: ReceiptOutcome::Ingested,
    }];
    receiver
        .ack(provider_info.node_id, &wire.package_id, receipts.clone())
        .await
        .unwrap();

    match recv_next(&mut provider_events).await {
        TransportEvent::AckReceived {
            from,
            package_id,
            receipts: got,
        } => {
            assert_eq!(from, receiver_info.node_id);
            assert_eq!(package_id, announce.package_id);
            assert_eq!(got, receipts);
        }
        other => panic!("expected AckReceived, got {other:?}"),
    }

    provider.shutdown().await;
    receiver.shutdown().await;
}

// ---------------------------------------------------------------------------
// 1b. Deterministic package tags: serve + fetch pin under `pkg/<id>`; release
//     deletes on both sides; a second release is idempotent.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn release_deletes_package_tags_on_both_sides() {
    use super::package_tag;

    let provider = mem_transport().await;
    let receiver = mem_transport().await;
    let (ip, ir) = start_and_pair(&provider, &receiver).await;
    let mut receiver_events = receiver.events().await;

    let tmp = tempdir().unwrap();
    let (dir, announce) =
        build_package(&tmp.path().join("src"), "uuid-gc-1", "gc.fits", "M1", 4096);
    provider.serve(&announce, &dir, None, None).await.unwrap();

    let tag = package_tag(&announce.package_id);
    // Provider pinned under the deterministic name.
    assert!(provider
        .store
        .tags()
        .get(tag.as_bytes())
        .await
        .unwrap()
        .is_some());

    // Announce so the receiver learns the iroh collection hash: the original
    // announce still carries only the xxh3 placeholder root_hash, and fetch
    // needs the wire announce. `package_id` is preserved, so the deterministic
    // tag name is unchanged on both sides.
    provider
        .announce(ir.node_id, &announce, "", "", &[], PackageLayout::Batch)
        .await
        .unwrap();
    let wire = match recv_next(&mut receiver_events).await {
        TransportEvent::AnnounceReceived { announce, .. } => announce,
        other => panic!("expected AnnounceReceived, got {other:?}"),
    };

    let dest = tempdir().unwrap();
    receiver
        .fetch(ip.node_id, &wire, dest.path(), noop_fetch_sink())
        .await
        .unwrap();
    // Receiver pinned the downloaded collection under the same name.
    assert!(receiver
        .store
        .tags()
        .get(tag.as_bytes())
        .await
        .unwrap()
        .is_some());

    provider.release(&announce.package_id).await.unwrap();
    receiver.release(&announce.package_id).await.unwrap();
    assert!(provider
        .store
        .tags()
        .get(tag.as_bytes())
        .await
        .unwrap()
        .is_none());
    assert!(receiver
        .store
        .tags()
        .get(tag.as_bytes())
        .await
        .unwrap()
        .is_none());

    // Idempotent second release.
    provider.release(&announce.package_id).await.unwrap();

    provider.shutdown().await;
    receiver.shutdown().await;
}

// ---------------------------------------------------------------------------
// 1c. Startup sweep: every tag present when a process starts is stale by
//     construction, so `start()` deletes them all before anything is served.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn start_sweeps_stale_tags() {
    use super::package_tag;

    // Persistent store so tags survive the restart (pattern from
    // iroh_resume_after_endpoint_restart, tests.rs:211).
    let home = tempfile::tempdir().unwrap();
    let t1 = IrohTransport::new(
        random_secret(),
        RelayMode::Disabled,
        BlobStore::Fs(home.path().to_path_buf()),
        None,
    )
    .await
    .unwrap();
    t1.start().await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (dir, announce) = build_package(tmp.path(), "uuid-sweep-1", "s.fits", "M1", 2048);
    t1.serve(&announce, &dir, None, None).await.unwrap();
    t1.shutdown().await;

    // New process over the same store: the old tag must be gone after start().
    let t2 = IrohTransport::new(
        random_secret(),
        RelayMode::Disabled,
        BlobStore::Fs(home.path().to_path_buf()),
        None,
    )
    .await
    .unwrap();
    t2.start().await.unwrap();
    let tag = package_tag(&announce.package_id);
    assert!(t2.store.tags().get(tag.as_bytes()).await.unwrap().is_none());
    t2.shutdown().await;
}

// ---------------------------------------------------------------------------
// 1d. Split sender/receiver blob dirs: a lazily-started second transport (the
//     sender half, over `blobs_out`) must NOT wipe the first's (the receiver
//     half, over `blobs`) live tags. Two `FsStore`s over ONE dir would: the
//     sender's startup `delete_all` sweep clears the receiver's pinned
//     `pkg/<id>`. Distinct dirs keep the two stores fully independent.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn split_blob_dirs_prevent_startup_sweep_interference() {
    use super::package_tag;

    let parent = tempfile::tempdir().unwrap();
    // The receiver half, over the production `blobs` dir.
    let receiver = IrohTransport::new(
        random_secret(),
        RelayMode::Disabled,
        BlobStore::Fs(parent.path().join("blobs")),
        None,
    )
    .await
    .unwrap();
    receiver.start().await.unwrap();

    // Serve a package on the receiver-half store so it pins a live `pkg/<id>`.
    let tmp = tempfile::tempdir().unwrap();
    let (dir, announce) = build_package(tmp.path(), "uuid-split-1", "split.fits", "M1", 2048);
    receiver.serve(&announce, &dir, None, None).await.unwrap();
    let tag = package_tag(&announce.package_id);
    assert!(
        receiver
            .store
            .tags()
            .get(tag.as_bytes())
            .await
            .unwrap()
            .is_some(),
        "receiver-half store pinned the package tag"
    );

    // The lazily-started sender half over the SEPARATE `blobs_out` dir. Its
    // startup sweep must only ever touch its own store.
    let sender = IrohTransport::new(
        random_secret(),
        RelayMode::Disabled,
        BlobStore::Fs(parent.path().join("blobs_out")),
        None,
    )
    .await
    .unwrap();
    sender.start().await.unwrap();

    // The receiver's live tag SURVIVES the sender's startup delete_all sweep —
    // this is the exact interference the dir split prevents.
    assert!(
        receiver
            .store
            .tags()
            .get(tag.as_bytes())
            .await
            .unwrap()
            .is_some(),
        "the sender half's startup sweep must not wipe the receiver half's live pkg tag"
    );

    sender.shutdown().await;
    receiver.shutdown().await;
}

// ---------------------------------------------------------------------------
// 2. Resume after endpoint restart over a persistent blob store.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn iroh_resume_after_endpoint_restart() {
    let tmp = tempdir().unwrap();
    let provider = mem_transport().await;
    let provider_info = provider.start().await.unwrap();

    // Receiver uses a PERSISTENT fs blob store so verified ranges survive a restart.
    let recv_home = tmp.path().join("recv_home");
    std::fs::create_dir_all(&recv_home).unwrap();
    let receiver = IrohTransport::new(
        random_secret(),
        RelayMode::Disabled,
        BlobStore::Fs(recv_home.clone()),
        None,
    )
    .await
    .unwrap();
    let receiver_info = receiver.start().await.unwrap();
    provider
        .add_peer_ticket(&receiver_info.pairing_ticket)
        .unwrap();
    receiver
        .add_peer_ticket(&provider_info.pairing_ticket)
        .unwrap();

    let mut receiver_events = receiver.events().await;

    // A multi-megabyte package, throttled below, so the first fetch is
    // interrupted mid-download — see the cancel block for why "likely" is not
    // good enough here.
    let (pkg_dir, announce) = build_package(
        &tmp.path().join("src"),
        "uuid-r",
        "big.fits",
        "M31",
        16 * 1024 * 1024,
    );
    provider
        .serve(&announce, &pkg_dir, None, None)
        .await
        .unwrap();
    provider
        .announce(
            receiver_info.node_id,
            &announce,
            "",
            "",
            &[],
            PackageLayout::Batch,
        )
        .await
        .unwrap();

    let wire = match recv_next(&mut receiver_events).await {
        TransportEvent::AnnounceReceived { announce, .. } => announce,
        other => panic!("expected AnnounceReceived, got {other:?}"),
    };

    let dest = tmp.path().join("dest");

    // The first attempt MUST be interrupted mid-download, and a wall clock
    // alone cannot promise that: a fast runner moves 16 MiB over loopback well
    // inside 80 ms. A first fetch that runs to completion does not merely make
    // the test vacuous — it makes it FAIL, because the export is
    // `ExportMode::TryReference` (transfer-prepare spec §5) and MOVES the
    // store's data file out to `dest`. The re-fetch then finds the entry
    // `Complete` with its file gone and takes the transfer-class
    // `on_export_source_vanished` branch, whose designed recovery is a tag drop
    // plus GC within fifteen minutes — not something a test can wait out. (The
    // sink cannot arm the cancel either: `FETCH_PROGRESS_MIN_INTERVAL` throttles
    // the first progress tick to 300 ms, later than the whole download.)
    //
    // So throttle the provider instead: at 4 MiB/s the payload needs about four
    // seconds, which puts the 80 ms cancel two orders of magnitude inside the
    // download, and lift the limit again so the resume runs at full speed.
    provider.set_upload_limit(4 * 1024 * 1024);
    let _ = tokio::time::timeout(
        Duration::from_millis(80),
        receiver.fetch(provider_info.node_id, &wire, &dest, noop_fetch_sink()),
    )
    .await;
    // The premise, asserted rather than assumed: export runs only after a blob
    // is fully downloaded, so an interrupted attempt leaves no file behind.
    assert!(
        !dest.join("big.fits").exists(),
        "first attempt was meant to be interrupted mid-download, but it finished"
    );
    provider.set_upload_limit(0);

    // Drop the receiving endpoint + store, releasing the fs blob dir.
    receiver.shutdown().await;

    // Recreate a fresh endpoint over the SAME persistent blob store and
    // re-fetch. This proves the operation completes and hash-verifies after a
    // restart over a persistent store; it does not itself measure bytes
    // re-transferred, so it is not proof that only the missing ranges moved —
    // genuine partial-range resume over a real interrupted transfer is what the
    // manual two-machine validation gate (task brief step 3) observes.
    let receiver2 = IrohTransport::new(
        random_secret(),
        RelayMode::Disabled,
        BlobStore::Fs(recv_home.clone()),
        None,
    )
    .await
    .unwrap();
    receiver2.start().await.unwrap();
    receiver2
        .add_peer_ticket(&provider_info.pairing_ticket)
        .unwrap();

    receiver2
        .fetch(provider_info.node_id, &wire, &dest, noop_fetch_sink())
        .await
        .expect("re-fetch after restart must complete");

    let fetched = dest.join("big.fits");
    assert!(fetched.exists(), "resumed payload missing");
    assert_eq!(
        xxh3_of(&pkg_dir.join("big.fits")),
        xxh3_of(&fetched),
        "resumed content must match source"
    );

    provider.shutdown().await;
    receiver2.shutdown().await;
}

// ---------------------------------------------------------------------------
// 3. The A4 engine over iroh: happy path + duplicate ack.
// ---------------------------------------------------------------------------

/// Spawn a reactive receiver over `receiver`: for each `AnnounceReceived`, fetch
/// into a fresh dir, ack every manifest frame as `Ingested` (twice when
/// `duplicate_ack`), then **release** the fetched blobs — mirroring the
/// production receiver ([`SyncReceiver`](crate::sync)'s post-ack release, task 3)
/// so the receiver's blob store returns to empty. Returns a slot holding the
/// last-seen package id so a caller can assert on its released tag.
fn spawn_iroh_receiver(
    receiver: Arc<IrohTransport>,
    dest_root: PathBuf,
    duplicate_ack: bool,
) -> Arc<std::sync::Mutex<Option<PackageId>>> {
    let captured: Arc<std::sync::Mutex<Option<PackageId>>> = Arc::new(std::sync::Mutex::new(None));
    let captured_ret = captured.clone();
    tokio::spawn(async move {
        let mut events = receiver.events().await;
        let mut n = 0usize;
        while let Some(ev) = events.recv().await {
            let TransportEvent::AnnounceReceived { from, announce, .. } = ev else {
                continue;
            };
            *captured.lock().unwrap() = Some(announce.package_id.clone());
            n += 1;
            let dest = dest_root.join(format!("fetch-{n}"));
            if receiver
                .fetch(from, &announce, &dest, noop_fetch_sink())
                .await
                .is_ok()
            {
                let records = match package::read_manifest(&dest) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let receipts: Vec<FrameReceipt> = records
                    .iter()
                    .map(|r| FrameReceipt {
                        frame_uuid: r.frame_uuid.clone(),
                        xxh3: r.xxh3.clone(),
                        outcome: ReceiptOutcome::Ingested,
                    })
                    .collect();
                let deliveries = if duplicate_ack { 2 } else { 1 };
                for _ in 0..deliveries {
                    let _ = receiver
                        .ack(from, &announce.package_id, receipts.clone())
                        .await;
                }
                // Post-ack release (idempotent), mirroring the real receiver.
                let _ = receiver.release(&announce.package_id).await;
            }
        }
    });
    captured_ret
}

#[tokio::test]
async fn engine_suite_over_iroh() {
    let tmp = tempdir().unwrap();
    let sender = Arc::new(mem_transport().await);
    let receiver = Arc::new(mem_transport().await);
    let (_sender_info, receiver_info) = start_and_pair(&sender, &receiver).await;
    let receiver_id = receiver_info.node_id;

    let captured = spawn_iroh_receiver(receiver.clone(), tmp.path().join("recv"), false);

    let (pkg_dir, _announce) = build_package(
        &tmp.path().join("src"),
        "uuid-e1",
        "frame_e1.fits",
        "M42",
        256 * 1024,
    );

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        sender.clone() as Arc<dyn SharingTransport>,
        receiver_id,
    );

    let id = engine
        .enqueue_package(&pkg_dir, None, Vec::new(), PackageLayout::Batch)
        .await
        .unwrap();
    wait_until(
        || state_of(&store, id) == Some(OutboundState::Confirmed),
        IROH_WAIT,
    )
    .await;

    let row = store.get_outbound(id).unwrap().unwrap();
    assert_eq!(row.state, OutboundState::Confirmed);
    assert!(row.confirmed_at.is_some(), "confirmed_at must be stamped");

    let history = store
        .search_history(HistoryQuery {
            filename: Some("frame_e1.fits".to_string()),
            object: None,
            direction: None,
            peer: None,
            project: None,
            package_id: None,
            limit: 100,
        })
        .unwrap();
    assert_eq!(
        history.len(),
        2,
        "history must record both the transfer-start and the confirm event"
    );
    assert!(history
        .iter()
        .any(|h| h.finished_at.is_none() && h.outcome == "sent"));
    assert!(history
        .iter()
        .any(|h| h.finished_at.is_some() && h.outcome == "ingested"));

    // Spec §6: after a confirmed transfer both blob stores are released — the
    // sender via the engine's confirm hook (fire-and-forget), the receiver via
    // its post-ack hook. Poll until the package tag is gone on both sides, then
    // assert nothing else remains (zero tags total).
    let pid = captured
        .lock()
        .unwrap()
        .clone()
        .expect("receiver must have captured the package id");
    let tag = super::package_tag(&pid);
    let deadline = Instant::now() + IROH_WAIT;
    loop {
        let sender_has = sender
            .store
            .tags()
            .get(tag.as_bytes())
            .await
            .unwrap()
            .is_some();
        let receiver_has = receiver
            .store
            .tags()
            .get(tag.as_bytes())
            .await
            .unwrap()
            .is_some();
        if !sender_has && !receiver_has {
            break;
        }
        if Instant::now() >= deadline {
            panic!("package tags not released after confirm: sender_has={sender_has} receiver_has={receiver_has}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        sender.store.tags().delete_all().await.unwrap(),
        0,
        "sender blob store must hold zero tags after confirm"
    );
    assert_eq!(
        receiver.store.tags().delete_all().await.unwrap(),
        0,
        "receiver blob store must hold zero tags after confirm"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn engine_dup_ack_confirms_once_over_iroh() {
    let tmp = tempdir().unwrap();
    let sender = Arc::new(mem_transport().await);
    let receiver = Arc::new(mem_transport().await);
    let (_sender_info, receiver_info) = start_and_pair(&sender, &receiver).await;
    let receiver_id = receiver_info.node_id;

    // Receiver acks twice (at-least-once): the engine must confirm exactly once.
    let _captured = spawn_iroh_receiver(receiver.clone(), tmp.path().join("recv"), true);

    let (pkg_dir, _announce) = build_package(
        &tmp.path().join("src"),
        "uuid-e2",
        "frame_e2.fits",
        "M13",
        128 * 1024,
    );

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        sender.clone() as Arc<dyn SharingTransport>,
        receiver_id,
    );

    let id = engine
        .enqueue_package(&pkg_dir, None, Vec::new(), PackageLayout::Batch)
        .await
        .unwrap();
    wait_until(
        || state_of(&store, id) == Some(OutboundState::Confirmed),
        IROH_WAIT,
    )
    .await;
    // Give the duplicate ack time to arrive and be (correctly) ignored.
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(state_of(&store, id), Some(OutboundState::Confirmed));
    let history = store
        .search_history(HistoryQuery {
            filename: Some("frame_e2.fits".to_string()),
            object: None,
            direction: None,
            peer: None,
            project: None,
            package_id: None,
            limit: 100,
        })
        .unwrap();
    let confirmed: Vec<_> = history.iter().filter(|h| h.finished_at.is_some()).collect();
    assert_eq!(
        confirmed.len(),
        1,
        "a duplicate ack must not produce a second confirm history row"
    );

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// 3b. Fix-review, production bug: a bare node id (no relay, no direct
//     addresses) is undialable — pins the exact production failure mode.
// ---------------------------------------------------------------------------

/// Required test #3: a peer registered with NO address (the pre-fix shape
/// account-mode resolution used to hand the transport) fails to dial with the
/// exact addressing error the production incident hit — `IrohTransport` binds
/// with `presets::Minimal` (no discovery services), so `endpoint.connect()` on
/// a bare `EndpointAddr` has nothing to try. Documents the invariant
/// `sync::pairing::peer_addr_with_relays` exists to satisfy: a bare node id
/// must never reach `add_peer`/`announce` without a relay (or direct address)
/// hint attached.
#[tokio::test]
async fn bare_node_id_without_a_peer_address_is_undialable() {
    let sender = mem_transport().await;
    let receiver = mem_transport().await;
    sender.start().await.unwrap();
    let receiver_info = receiver.start().await.unwrap();

    // Deliberately skip add_peer/add_peer_ticket: `receiver_info.node_id` is a
    // bare identity with no registered address — exactly the pre-fix
    // account-mode resolution's shape.
    let tmp = tempdir().unwrap();
    let (pkg_dir, announce) = build_package(
        &tmp.path().join("src"),
        "uuid-bare",
        "frame_bare.fits",
        "M1",
        4096,
    );
    sender.serve(&announce, &pkg_dir, None, None).await.unwrap();

    let err = sender
        .announce(
            receiver_info.node_id,
            &announce,
            "",
            "",
            &[],
            PackageLayout::Batch,
        )
        .await
        .expect_err("a bare node id with no relay/direct address must fail to dial");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("addressing information") || msg.contains("address lookup"),
        "error should name the addressing failure (the production symptom), got: {msg}"
    );

    sender.shutdown().await;
    receiver.shutdown().await;
}

// ---------------------------------------------------------------------------
// 4. Path-traversal guard: a peer-supplied collection entry name must never
//    escape dest_dir. Mirrors package::validate_rel_path on the write side.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fetch_rejects_traversal_entry_names() {
    let provider = mem_transport().await;
    let receiver = mem_transport().await;
    let (provider_info, _receiver_info) = start_and_pair(&provider, &receiver).await;

    // Build a malicious collection directly via the blobs API — bypassing
    // write_package, which already guards `rel_path` on the write side. The
    // point of this test is that a *peer* controls collection entry names, and
    // nothing on the write side can stop them from sending anything.
    let tt = provider
        .store
        .blobs()
        .add_bytes(b"malicious payload".to_vec())
        .temp_tag()
        .await
        .unwrap();

    // An absolute path under the OS temp dir: writable in practice (unlike
    // /etc), so a vulnerable implementation would actually write there,
    // proving the escape rather than merely failing on a permission error.
    let abs_target = std::env::temp_dir().join(format!(
        "athenaeum_a5_traversal_probe_{}.bin",
        uuid::Uuid::new_v4()
    ));
    let items = vec![
        ("../escape_relative.bin".to_string(), tt.hash()),
        (abs_target.to_string_lossy().to_string(), tt.hash()),
    ];
    let collection = Collection::from_iter(items);
    let collection_tag = collection.store(&provider.store).await.unwrap();
    provider
        .store
        .tags()
        .create(collection_tag.hash_and_format())
        .await
        .unwrap();
    let root_hash = collection_tag.hash();

    let dest_parent = tempdir().unwrap();
    let dest = dest_parent.path().join("dest");
    let provider_id = EndpointId::from_bytes(&provider_info.node_id).unwrap();

    let err = super::blobs::fetch_collection_to_dir(
        &receiver.store,
        &receiver.endpoint,
        provider_id,
        root_hash,
        "pkg/traversal-probe",
        &dest,
        0,
        noop_fetch_sink(),
    )
    .await
    .expect_err("a malicious collection entry name must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("rel_path") || msg.contains("validation"),
        "error should name the path-validation failure, got: {msg}"
    );

    // Nothing must have been written anywhere: validation runs before dest_dir
    // is even created, so neither the traversal entry nor the absolute-path
    // entry — nor dest_dir itself — should exist on disk.
    assert!(
        !dest.exists(),
        "dest_dir must not be created on a rejected entry"
    );
    assert!(
        !dest_parent.path().join("escape_relative.bin").exists(),
        "traversal entry must not write outside dest_dir"
    );
    assert!(
        !abs_target.exists(),
        "absolute-path entry must not write to an arbitrary path"
    );

    // Task 2.3: the fetch set the in-flight GC-protect tag right after phase 1
    // yielded the root, and a fetch that errors (here on name validation) KEEPS
    // it — an interrupted/errored fetch's partial data must stay GC-protected
    // until a resume, not lose its tag on the error path. (Pre-fix there is no
    // such tag at all, so this asserts the new set-and-keep behavior.)
    let in_flight = super::blobs::in_flight_tag("pkg/traversal-probe");
    assert!(
        receiver
            .store
            .tags()
            .get(in_flight.as_bytes())
            .await
            .unwrap()
            .is_some(),
        "an errored fetch must retain the in-flight download tag for a later resume"
    );

    provider.shutdown().await;
    receiver.shutdown().await;
}

// ---------------------------------------------------------------------------
// 4b. Connection-path diagnostics (NAT investigation): the establishment line
//     fires with a `conn_type` field. Relay is disabled here, so the in-process
//     localhost connection classifies as `direct`. A minimal thread-local
//     capture layer asserts the field without touching the global JSONL
//     subscriber (the logging suite owns that) — `#[tokio::test]` runs on a
//     current-thread runtime, so the inline outgoing dial and the spawned
//     inbound accept both execute on this thread and are captured.
// ---------------------------------------------------------------------------

/// One captured tracing event: its message plus its string-rendered fields.
#[derive(Clone, Default)]
struct CapturedEvent {
    message: String,
    fields: std::collections::HashMap<String, String>,
}

/// Field visitor: records `message` separately and every other field as a
/// string, covering both `record_str` (e.g. `conn_type = "direct"`) and
/// `record_debug` (e.g. `%peer`, and the format_args message).
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

/// A minimal `tracing` layer capturing this module's events into a shared vec.
/// Filtered to `athenaeum_core::sharing` targets (iroh's own high-volume events
/// are dropped cheaply) and hinted to `INFO` so debug/trace callsites never even
/// fire.
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

#[tokio::test]
async fn connection_path_established_line_carries_conn_type_field() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let captured: Arc<std::sync::Mutex<Vec<CapturedEvent>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let layer = CaptureLayer {
        events: captured.clone(),
    };
    // Thread-local default (not global): takes precedence on this thread over any
    // global subscriber a neighbouring test installed, and the current-thread
    // runtime keeps every task on this thread, so all establishment lines land.
    let _guard = tracing_subscriber::registry().with(layer).set_default();

    let provider = mem_transport().await;
    let receiver = mem_transport().await;
    let (_provider_info, receiver_info) = start_and_pair(&provider, &receiver).await;
    let mut receiver_events = receiver.events().await;

    let tmp = tempdir().unwrap();
    let (pkg_dir, announce) = build_package(
        &tmp.path().join("src"),
        "uuid-path-1",
        "path1.fits",
        "M1",
        4096,
    );
    provider
        .serve(&announce, &pkg_dir, None, None)
        .await
        .unwrap();
    // `announce` dials an outgoing control connection (logged inline on this
    // task) AND drives the receiver's inbound accept (logged on its own task) —
    // both emit the establishment line.
    provider
        .announce(
            receiver_info.node_id,
            &announce,
            "",
            "",
            &[],
            PackageLayout::Batch,
        )
        .await
        .unwrap();
    // Await dispatch so the inbound accept path has certainly run before we read.
    match recv_next(&mut receiver_events).await {
        TransportEvent::AnnounceReceived { .. } => {}
        other => panic!("expected AnnounceReceived, got {other:?}"),
    }

    let events = captured.lock().unwrap();
    let established: Vec<&CapturedEvent> = events
        .iter()
        .filter(|e| e.message == "connection path established")
        .collect();
    assert!(
        !established.is_empty(),
        "expected a 'connection path established' line; captured messages: {:?}",
        events.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
    );
    assert!(
        established
            .iter()
            .all(|e| e.fields.contains_key("conn_type")),
        "every establishment line must carry a conn_type field; got: {:?}",
        established
            .iter()
            .map(|e| e.fields.clone())
            .collect::<Vec<_>>()
    );
    // Relay disabled + localhost ⇒ at least one path classifies as a direct IP path.
    assert!(
        established
            .iter()
            .any(|e| e.fields.get("conn_type").map(String::as_str) == Some("direct")),
        "an in-process localhost connection (relay disabled) must classify as direct; got: {:?}",
        established
            .iter()
            .map(|e| e.fields.clone())
            .collect::<Vec<_>>()
    );
    drop(events);

    provider.shutdown().await;
    receiver.shutdown().await;
}

// ---------------------------------------------------------------------------
// 5. Dedup handshake over iroh: negotiate_want drives Offer→Want→FullHashes→Want
//    against a responder wired into the receiver's control channel.
// ---------------------------------------------------------------------------

use crate::sharing::iroh::proto::{FullHashEntry, OfferEntry};
use crate::sync::DedupResponder;
use std::collections::{HashMap, HashSet};

/// Catalog-free responder mirroring the loopback test's stub: `present` are the
/// sampling hashes it "has", `local_full` maps each to the full xxh3 of its
/// local file so a true-duplicate full-hash match can be settled.
struct StubResponder {
    present: HashSet<String>,
    local_full: HashMap<String, String>,
}

impl DedupResponder for StubResponder {
    fn want_for_offer(&self, entries: &[OfferEntry]) -> (Vec<String>, Vec<String>) {
        let mut want = Vec::new();
        let mut cands = Vec::new();
        for e in entries {
            if self.present.contains(&e.sampling_hash) {
                cands.push(e.rel_path.clone());
            } else {
                want.push(e.rel_path.clone());
            }
        }
        (want, cands)
    }

    fn confirm_full_hashes(&self, entries: &[FullHashEntry]) -> Vec<String> {
        entries
            .iter()
            .filter(|e| match self.local_full.get(&e.sampling_hash) {
                Some(local) => local != &e.xxh3_full,
                None => true,
            })
            .map(|e| e.rel_path.clone())
            .collect()
    }
}

fn oe(rel: &str, sampling: &str) -> OfferEntry {
    OfferEntry {
        rel_path: rel.into(),
        sampling_hash: sampling.into(),
        byte_size: 1,
    }
}

#[tokio::test]
async fn iroh_negotiate_returns_only_absent_and_false_positive_wants() {
    let responder = StubResponder {
        present: ["have".to_string()].into_iter().collect(),
        local_full: [("have".to_string(), "F_HAVE".to_string())]
            .into_iter()
            .collect(),
    };
    let receiver = mem_transport_with_responder(Arc::new(responder)).await;
    let sender = mem_transport().await;
    let (_sender_info, receiver_info) = start_and_pair(&sender, &receiver).await;

    let offer = vec![
        oe("new.fits", "absent"),   // absent sampling → want
        oe("have.fits", "have"),    // candidate, full matches → drop
        oe("collide.fits", "have"), // candidate, full differs → false positive → want
    ];
    let full: HashMap<String, String> = [
        ("have.fits".to_string(), "F_HAVE".to_string()),
        ("collide.fits".to_string(), "F_OTHER".to_string()),
    ]
    .into_iter()
    .collect();

    let want = sender
        .negotiate_want(receiver_info.node_id, PackageId("p".into()), offer, full)
        .await
        .expect("negotiate_want over iroh must succeed");
    assert_eq!(
        want,
        ["new.fits".to_string(), "collide.fits".to_string()]
            .into_iter()
            .collect::<HashSet<String>>()
    );

    sender.shutdown().await;
    receiver.shutdown().await;
}

#[tokio::test]
async fn iroh_negotiate_without_responder_wants_everything() {
    // A responder-less receiver still answers Offer — with want-all, so a full
    // peer that never wired a catalog receives every offered frame.
    let receiver = mem_transport().await;
    let sender = mem_transport().await;
    let (_sender_info, receiver_info) = start_and_pair(&sender, &receiver).await;

    let offer = vec![oe("a.fits", "h1"), oe("b.fits", "h2")];
    let want = sender
        .negotiate_want(
            receiver_info.node_id,
            PackageId("p".into()),
            offer,
            HashMap::new(),
        )
        .await
        .expect("negotiate_want must succeed against a responder-less peer");
    assert_eq!(
        want,
        ["a.fits".to_string(), "b.fits".to_string()]
            .into_iter()
            .collect::<HashSet<String>>()
    );

    sender.shutdown().await;
    receiver.shutdown().await;
}

// ---------------------------------------------------------------------------
// 6. Want-subset collection: a subset serve builds a collection from only the
//    negotiated frames + a manifest filtered to them, so fetch lands exactly
//    those frames.
// ---------------------------------------------------------------------------

/// Build a 3-frame package (`frame1/2/3.fits` + manifest) and return
/// `(pkg_dir, announce, [rel_path…])`. Distinct per-frame content so a
/// wrong-frame transfer would hash-mismatch.
fn build_three_frame_package(src_root: &Path) -> (PathBuf, PackageAnnounce, Vec<String>) {
    std::fs::create_dir_all(src_root).unwrap();
    let mut records: Vec<(PathBuf, ManifestRecord)> = Vec::new();
    let mut rels = Vec::new();
    for i in 1..=3u8 {
        let filename = format!("frame{i}.fits");
        let payload = src_root.join(&filename);
        let bytes: Vec<u8> = (0..(4096 + i as usize * 100))
            .map(|j| ((j + i as usize) % 251) as u8)
            .collect();
        std::fs::write(&payload, &bytes).unwrap();
        let byte_size = std::fs::metadata(&payload).unwrap().len();
        let xxh3 = package::xxh3_full_file(&payload).unwrap();
        records.push((
            payload,
            ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: format!("uuid-{i}"),
                origin_catalog_uuid: "catalog-uuid".to_string(),
                origin_device: "origin-device".to_string(),
                payload_kind: PayloadKind::RawFrame,
                rel_path: filename.clone(),
                byte_size,
                xxh3,
                frame_meta: serde_json::json!({ "n": i }),
                analysis: None,
                app_version: "test".to_string(),
                project: None,
            },
        ));
        rels.push(filename);
    }
    let pkg_dir = src_root.parent().unwrap().join("pkg-three");
    let announce = write_package(&pkg_dir, records).unwrap();
    (pkg_dir, announce, rels)
}

#[tokio::test]
async fn subset_serve_transfers_only_want_frames() {
    let provider = mem_transport().await;
    let receiver = mem_transport().await;
    let (provider_info, receiver_info) = start_and_pair(&provider, &receiver).await;
    let mut receiver_events = receiver.events().await;

    let tmp = tempdir().unwrap();
    let (pkg_dir, announce, rels) = build_three_frame_package(&tmp.path().join("src"));

    // Want only frame1 + frame3 (frame2 already held by the peer).
    let want: HashSet<String> = [rels[0].clone(), rels[2].clone()].into_iter().collect();
    provider
        .serve(&announce, &pkg_dir, Some(&want), None)
        .await
        .unwrap();
    provider
        .announce(
            receiver_info.node_id,
            &announce,
            "",
            "",
            &[],
            PackageLayout::Batch,
        )
        .await
        .unwrap();

    // The wire announce carries the subset collection hash.
    let wire = match recv_next(&mut receiver_events).await {
        TransportEvent::AnnounceReceived { announce, .. } => announce,
        other => panic!("expected AnnounceReceived, got {other:?}"),
    };

    let dest = tempdir().unwrap();
    receiver
        .fetch(provider_info.node_id, &wire, dest.path(), noop_fetch_sink())
        .await
        .unwrap();

    assert!(
        dest.path().join(&rels[0]).exists(),
        "frame1 (wanted) present"
    );
    assert!(
        !dest.path().join(&rels[1]).exists(),
        "frame2 (not wanted) must be absent from the subset collection"
    );
    assert!(
        dest.path().join(&rels[2]).exists(),
        "frame3 (wanted) present"
    );

    // The downloaded manifest holds exactly the two wanted records.
    let fetched = package::read_manifest(dest.path()).unwrap();
    assert_eq!(fetched.len(), 2, "filtered manifest must hold 2 records");
    let got: HashSet<String> = fetched.iter().map(|r| r.rel_path.clone()).collect();
    assert_eq!(
        got, want,
        "manifest records must be exactly the wanted rel_paths"
    );

    // The wanted payloads round-trip byte-for-byte.
    assert_eq!(
        xxh3_of(&pkg_dir.join(&rels[0])),
        xxh3_of(&dest.path().join(&rels[0])),
        "frame1 content mismatch"
    );

    provider.shutdown().await;
    receiver.shutdown().await;
}

/// An empty want set is a caller error — Task 6 drops an all-duplicate package
/// before serve, so `serve(Some(empty))` must fail loudly rather than build a
/// zero-frame collection.
#[tokio::test]
async fn subset_serve_empty_want_is_error() {
    let provider = mem_transport().await;
    provider.start().await.unwrap();

    let tmp = tempdir().unwrap();
    let (pkg_dir, announce, _rels) = build_three_frame_package(&tmp.path().join("src"));

    let empty: HashSet<String> = HashSet::new();
    let err = provider
        .serve(&announce, &pkg_dir, Some(&empty), None)
        .await
        .expect_err("an empty want set must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("empty want"),
        "error should name the empty-want guard, got: {msg}"
    );

    provider.shutdown().await;
}

// ---------------------------------------------------------------------------
// 7. Connect gate (collab exchange, slice 4): an ungated peer gets no control
//    dispatch and no blob bytes. `authz.rs`'s unit tests and the receiver's
//    loopback gate test cover the DECISION (authz.rs) and the WIRING
//    (ReceiverHooks → ensure_started → project_gate); neither drives the
//    loopback mock, so this file is the only coverage of the actual iroh
//    `ProtocolHandler` refusal — `connect_gate_admits`, the
//    `connection.close(..., b"unauthorized")` call, and both
//    `SyncControlProtocol::accept` and `GatedBlobs::accept` refusing before
//    delegating.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connect_gate_refuses_control_dispatch_and_blocks_announce() {
    let provider = mem_transport().await;
    let receiver = mem_transport().await;
    let (_provider_info, receiver_info) = start_and_pair(&provider, &receiver).await;

    // Installed AFTER start/pair: `set_connect_gate` is late-bindable (a mutexed
    // slot cloned into the handlers at construction), so an already-spawned
    // router picks up this predicate on the very next connection.
    receiver.set_connect_gate(Arc::new(|_from: &NodeId| false));

    let mut receiver_events = receiver.events().await;

    let tmp = tempdir().unwrap();
    let (pkg_dir, announce) = build_package(
        &tmp.path().join("src"),
        "uuid-gate-1",
        "gate1.fits",
        "M1",
        4096,
    );
    provider
        .serve(&announce, &pkg_dir, None, None)
        .await
        .unwrap();

    // The refusing gate closes the connection at the TOP of
    // `SyncControlProtocol::accept` — before a single `Msg` is decoded — so the
    // sender never receives its one-byte delivery ack and `announce` errors.
    // Bounded well under `CONTROL_SEND_TIMEOUT` (30s): an actively closed
    // connection fails the pending read almost immediately, never waits it out.
    let outcome = tokio::time::timeout(
        Duration::from_secs(15),
        provider.announce(
            receiver_info.node_id,
            &announce,
            "",
            "",
            &[],
            PackageLayout::Batch,
        ),
    )
    .await;
    match outcome {
        Ok(Ok(())) => panic!("announce must not succeed against a refusing connect gate"),
        Ok(Err(_)) | Err(_) => {} // expected: no delivery ack, ever
    }

    // And zero control dispatch: no `AnnounceReceived` was ever pushed onto the
    // receiver's event stream, because the control loop never ran at all.
    let never_arrived =
        tokio::time::timeout(Duration::from_millis(300), receiver_events.recv()).await;
    assert!(
        never_arrived.is_err(),
        "a gated peer must produce zero control dispatch, but an event arrived"
    );

    provider.shutdown().await;
    receiver.shutdown().await;
}

/// Companion to the refusal test above — proves the refusal there is caused by
/// the gate's verdict, not by broken test setup (e.g. an uncloned gate slot, an
/// inverted boolean that always refuses): the SAME announce, over the SAME two
/// endpoints, succeeds end to end when the installed predicate returns `true`.
#[tokio::test]
async fn connect_gate_permits_when_predicate_allows() {
    let provider = mem_transport().await;
    let receiver = mem_transport().await;
    let (provider_info, receiver_info) = start_and_pair(&provider, &receiver).await;

    receiver.set_connect_gate(Arc::new(|_from: &NodeId| true));

    let mut receiver_events = receiver.events().await;
    let tmp = tempdir().unwrap();
    let (pkg_dir, announce) = build_package(
        &tmp.path().join("src"),
        "uuid-gate-2",
        "gate2.fits",
        "M1",
        4096,
    );
    provider
        .serve(&announce, &pkg_dir, None, None)
        .await
        .unwrap();

    provider
        .announce(
            receiver_info.node_id,
            &announce,
            "",
            "",
            &[],
            PackageLayout::Batch,
        )
        .await
        .expect("a permitting connect gate must not block the announce");

    match recv_next(&mut receiver_events).await {
        TransportEvent::AnnounceReceived { from, .. } => assert_eq!(from, provider_info.node_id),
        other => panic!("expected AnnounceReceived, got {other:?}"),
    }

    provider.shutdown().await;
    receiver.shutdown().await;
}

/// The blob-content half of the same contract: `GatedBlobs::accept` must refuse
/// a download connection exactly like `SyncControlProtocol::accept` refuses a
/// control connection. The gate lives on whichever side SERVES blob content —
/// here the provider, since the receiver dials IN to download — so the
/// announce is delivered first (learning the real collection hash), and only
/// then is the provider gated shut for the fetch.
#[tokio::test]
async fn connect_gate_refuses_blob_fetch() {
    let provider = mem_transport().await;
    let receiver = mem_transport().await;
    let (provider_info, receiver_info) = start_and_pair(&provider, &receiver).await;
    let mut receiver_events = receiver.events().await;

    let tmp = tempdir().unwrap();
    let (pkg_dir, announce) = build_package(
        &tmp.path().join("src"),
        "uuid-gate-3",
        "gate3.fits",
        "M1",
        4096,
    );
    provider
        .serve(&announce, &pkg_dir, None, None)
        .await
        .unwrap();
    // Deliver the announce BEFORE gating the provider, so the receiver learns
    // the real iroh collection hash — this test's point is the BLOB path, not
    // the control path (already covered above).
    provider
        .announce(
            receiver_info.node_id,
            &announce,
            "",
            "",
            &[],
            PackageLayout::Batch,
        )
        .await
        .unwrap();
    let wire = match recv_next(&mut receiver_events).await {
        TransportEvent::AnnounceReceived { announce, .. } => announce,
        other => panic!("expected AnnounceReceived, got {other:?}"),
    };

    // NOW gate the provider — the side whose `GatedBlobs` wrapper must refuse
    // the receiver's incoming download connection before ever delegating to the
    // inner `iroh_blobs` handler.
    provider.set_connect_gate(Arc::new(|_from: &NodeId| false));

    let dest_parent = tempdir().unwrap();
    let dest = dest_parent.path().join("dest");
    let outcome = tokio::time::timeout(
        Duration::from_secs(15),
        receiver.fetch(provider_info.node_id, &wire, &dest, noop_fetch_sink()),
    )
    .await;
    match outcome {
        Ok(Ok(())) => panic!("fetch must not succeed against a refusing connect gate"),
        Ok(Err(_)) | Err(_) => {} // expected: no blob bytes ever delivered
    }
    assert!(
        !dest.exists(),
        "no blob bytes land: dest_dir is only created after a successful download"
    );

    provider.shutdown().await;
    receiver.shutdown().await;
}

// ---------------------------------------------------------------------------
// 5. Task 2.3 — in-flight GC-protect tag (the live-bug fix). A collection whose
//    root + children sit in the store under the in-flight hash_seq tag survives
//    a REAL garbage-collection sweep, while an identically-shaped but untagged
//    collection is collected. The survivor still loads + exports intact, i.e. a
//    resumed fetch completes from the retained data instead of restarting from
//    zero. Driven over a real FsStore with a short GC interval so the store's own
//    background GC loop actually runs — no re-implemented GC.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn in_flight_tag_protects_partial_collection_across_gc() {
    use iroh_blobs::api::Store;
    use iroh_blobs::store::fs::options::Options as FsOptions;
    use iroh_blobs::store::fs::FsStore;
    use iroh_blobs::store::GcConfig;

    let home = tempdir().unwrap();
    let blob_dir = home.path().join("sync_blobs");
    std::fs::create_dir_all(&blob_dir).unwrap();
    // Real fs store with a short GC interval (production is 900 s). The GC loop
    // sleeps `interval` before its first mark, and every blob below is held by a
    // temp tag until its permanent protection (or lack thereof) is in place, so
    // there is no unprotected setup window to race.
    let mut options = FsOptions::new(&blob_dir);
    options.gc = Some(GcConfig {
        interval: Duration::from_millis(500),
        add_protected: None,
    });
    let store: Store = FsStore::load_with_opts(blob_dir.join("blobs.db"), options)
        .await
        .unwrap()
        .into();

    // PROTECTED collection: two children + a hash-seq root, pinned by the
    // in-flight tag (hash_seq format). Child temp tags stay alive until the
    // permanent in-flight tag is set, so the collection is never unprotected.
    let pa = store
        .blobs()
        .add_bytes(vec![0xA1u8; 8192])
        .temp_tag()
        .await
        .unwrap();
    let pb = store
        .blobs()
        .add_bytes(vec![0xB2u8; 8192])
        .temp_tag()
        .await
        .unwrap();
    let (pa_h, pb_h) = (pa.hash(), pb.hash());
    let prot = Collection::from_iter([("a.fits".to_string(), pa_h), ("b.fits".to_string(), pb_h)]);
    let prot_root_tt = prot.store(&store).await.unwrap();
    let prot_root = prot_root_tt.hash();
    let in_flight = super::blobs::in_flight_tag("recv/pkg/protected");
    store
        .tags()
        .set(&in_flight, prot_root_tt.hash_and_format())
        .await
        .unwrap();
    drop((pa, pb, prot_root_tt));

    // CONTROL collection: same shape, NO tag → must be swept. Its temp tags are
    // dropped here, before the canary is added, so a sweep that removes the canary
    // necessarily saw (and swept) the control too.
    let ca = store
        .blobs()
        .add_bytes(vec![0xC3u8; 8192])
        .temp_tag()
        .await
        .unwrap();
    let cb = store
        .blobs()
        .add_bytes(vec![0xD4u8; 8192])
        .temp_tag()
        .await
        .unwrap();
    let (ca_h, cb_h) = (ca.hash(), cb.hash());
    let ctrl = Collection::from_iter([("a.fits".to_string(), ca_h), ("b.fits".to_string(), cb_h)]);
    let ctrl_root_tt = ctrl.store(&store).await.unwrap();
    let ctrl_root = ctrl_root_tt.hash();
    drop((ca, cb, ctrl_root_tt));

    // Canary: an untagged blob added LAST. Its disappearance is the deterministic
    // signal that a full GC mark+sweep pass has run over the final store state.
    let canary_tt = store
        .blobs()
        .add_bytes(b"canary".to_vec())
        .temp_tag()
        .await
        .unwrap();
    let canary = canary_tt.hash();
    drop(canary_tt);

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if !store.blobs().has(canary).await.unwrap() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "GC did not run within the timeout"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The in-flight hash_seq tag kept the whole protected collection live...
    assert!(
        store.blobs().has(prot_root).await.unwrap(),
        "protected root survives GC"
    );
    assert!(
        store.blobs().has(pa_h).await.unwrap(),
        "protected child a survives GC"
    );
    assert!(
        store.blobs().has(pb_h).await.unwrap(),
        "protected child b survives GC"
    );
    // ...while the untagged control collection was collected.
    assert!(
        !store.blobs().has(ctrl_root).await.unwrap(),
        "untagged control root swept"
    );
    assert!(
        !store.blobs().has(ca_h).await.unwrap(),
        "untagged control child a swept"
    );
    assert!(
        !store.blobs().has(cb_h).await.unwrap(),
        "untagged control child b swept"
    );

    // Resume completes from the RETAINED data: the collection reloads and every
    // child exports intact — a resumed fetch would find nothing left to pull.
    let loaded = Collection::load(prot_root, &store).await.unwrap();
    assert_eq!(
        loaded.len(),
        2,
        "retained collection reloads with both entries"
    );
    let out_dir = home.path().join("resume_out");
    std::fs::create_dir_all(&out_dir).unwrap();
    for (name, hash) in loaded.iter() {
        store
            .blobs()
            .export(*hash, out_dir.join(name))
            .await
            .unwrap();
    }
    assert_eq!(
        std::fs::read(out_dir.join("a.fits")).unwrap(),
        vec![0xA1u8; 8192]
    );
    assert_eq!(
        std::fs::read(out_dir.join("b.fits")).unwrap(),
        vec![0xB2u8; 8192]
    );
}

// ---------------------------------------------------------------------------
// 6. Task 2.3 — a SUCCESSFUL fetch retires the in-flight tag. After a full
//    fetch, the permanent package tag pins the collection and NO in-flight tag
//    is left behind (it is deleted right after the permanent tag is set).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn successful_fetch_clears_in_flight_tag() {
    use super::package_tag;

    let tmp = tempdir().unwrap();
    let provider = mem_transport().await;
    let receiver = mem_transport().await;
    let (provider_info, receiver_info) = start_and_pair(&provider, &receiver).await;
    let mut receiver_events = receiver.events().await;

    let (pkg_dir, announce) = build_package(
        &tmp.path().join("src"),
        "uuid-if",
        "if.fits",
        "M27",
        64 * 1024,
    );
    provider
        .serve(&announce, &pkg_dir, None, None)
        .await
        .unwrap();
    provider
        .announce(
            receiver_info.node_id,
            &announce,
            "",
            "",
            &[],
            PackageLayout::Batch,
        )
        .await
        .unwrap();

    let wire = match recv_next(&mut receiver_events).await {
        TransportEvent::AnnounceReceived { announce, .. } => announce,
        other => panic!("expected AnnounceReceived, got {other:?}"),
    };

    let dest = tmp.path().join("dest");
    receiver
        .fetch(provider_info.node_id, &wire, &dest, noop_fetch_sink())
        .await
        .expect("fetch must complete");

    // Success invariant: permanent tag present, in-flight tag gone (no leak).
    let permanent = package_tag(&wire.package_id);
    let in_flight = super::blobs::in_flight_tag(&permanent);
    assert!(
        receiver
            .store
            .tags()
            .get(permanent.as_bytes())
            .await
            .unwrap()
            .is_some(),
        "permanent package tag present after a successful fetch"
    );
    assert!(
        receiver
            .store
            .tags()
            .get(in_flight.as_bytes())
            .await
            .unwrap()
            .is_none(),
        "in-flight tag must be gone after a successful fetch (no leak)"
    );

    provider.shutdown().await;
    receiver.shutdown().await;
}

// ---------------------------------------------------------------------------
// W1: provider throttle hook (ThrottleMode::Intercept).
// ---------------------------------------------------------------------------

/// REGRESSION PIN for the deadly-drop arm. With `ThrottleMode::Intercept` on,
/// the provider awaits an rpc reply from our consumer for EVERY ~16 KiB payload
/// write — a dropped reply channel errors the writer and ABORTS the peer's
/// download. The consumer's old `ProviderMessage::Throttle(_) => {}` arm did
/// exactly that drop; this test transfers a multi-chunk package at rate 0
/// (unlimited) over the REAL iroh stack and fails against that arm the moment
/// the mask flips. Loopback e2e cannot cover this: the mock transport bypasses
/// iroh-blobs entirely, so this real-QUIC-localhost test is the only pin.
#[tokio::test]
async fn throttle_intercept_zero_rate_transfer_completes() {
    let provider = mem_transport().await;
    let receiver = mem_transport().await;
    let (provider_info, receiver_info) = start_and_pair(&provider, &receiver).await;
    let mut receiver_events = receiver.events().await;

    // ~600 KiB ⇒ dozens of 16 KiB payload chunks ⇒ many Throttle round trips.
    let tmp = tempdir().unwrap();
    let (pkg_dir, announce) = build_package(
        &tmp.path().join("src-throttle"),
        "uuid-throttle",
        "frame_throttle.fits",
        "M42",
        600 * 1024,
    );

    provider
        .serve(&announce, &pkg_dir, None, None)
        .await
        .unwrap();
    provider
        .announce(
            receiver_info.node_id,
            &announce,
            "",
            "",
            &[],
            PackageLayout::Batch,
        )
        .await
        .unwrap();
    let wire = match recv_next(&mut receiver_events).await {
        TransportEvent::AnnounceReceived { announce, .. } => announce,
        other => panic!("expected AnnounceReceived, got {other:?}"),
    };

    let dest = tempdir().unwrap();
    receiver
        .fetch(provider_info.node_id, &wire, dest.path(), noop_fetch_sink())
        .await
        .expect(
            "a zero-rate (unlimited) transfer must complete — an unreplied Throttle rpc aborts it",
        );
    assert_eq!(
        xxh3_of(&pkg_dir.join("frame_throttle.fits")),
        xxh3_of(&dest.path().join("frame_throttle.fits")),
        "content intact through the intercept path"
    );

    provider.shutdown().await;
    receiver.shutdown().await;
}

/// END-TO-END PIN that a delayed Throttle reply genuinely SLOWS a transfer —
/// not just that it survives one.
///
/// Coverage split, stated honestly:
/// - the loopback e2e harness (`sharing::tests`) bypasses iroh-blobs entirely,
///   so it can never exercise the throttle hook at all;
/// - the pacer unit tests (`super::pacer`) pin the delay ARITHMETIC over a
///   virtual clock, but never sleep and never touch a provider;
/// - [`throttle_intercept_zero_rate_transfer_completes`] pins the *other*
///   direction — unlimited still completes (the deadly-drop regression).
///
/// THIS test is the only place where the whole chain is real: a rate is set on
/// the provider, the consumer loop sleeps the pacer's delay before replying to
/// the `Throttle` rpc, and the provider's writer — which awaits that reply
/// inline per ~16 KiB chunk — is therefore actually paused. If the sleep were
/// dropped, or the reply sent eagerly, or the pacer never consulted, the
/// transfer would finish at localhost speed and this test would fail.
#[tokio::test]
async fn upload_pacer_limits_real_transfer_wall_clock() {
    let provider = mem_transport().await;
    let receiver = mem_transport().await;
    let (provider_info, receiver_info) = start_and_pair(&provider, &receiver).await;
    let mut receiver_events = receiver.events().await;

    let tmp = tempdir().unwrap();
    let (pkg_dir, announce) = build_package(
        &tmp.path().join("src-paced"),
        "uuid-paced",
        "frame_paced.fits",
        "M42",
        3 * 1024 * 1024,
    );

    provider
        .serve(&announce, &pkg_dir, None, None)
        .await
        .unwrap();
    provider
        .announce(
            receiver_info.node_id,
            &announce,
            "",
            "",
            &[],
            PackageLayout::Batch,
        )
        .await
        .unwrap();
    let wire = match recv_next(&mut receiver_events).await {
        TransportEvent::AnnounceReceived { announce, .. } => announce,
        other => panic!("expected AnnounceReceived, got {other:?}"),
    };

    let dest = tempdir().unwrap();

    // 1 MB/s, armed on the PROVIDER before the receiver pulls a single byte —
    // the pacer only governs the upload side.
    provider.set_upload_limit(1_000_000);

    // Time ONLY the fetch: serve, announce and pairing above are unpaced setup
    // whose cost would muddy the floor below.
    let t0 = std::time::Instant::now();
    receiver
        .fetch(provider_info.node_id, &wire, dest.path(), noop_fetch_sink())
        .await
        .expect("a paced transfer must still COMPLETE, only slower");
    let elapsed = t0.elapsed();

    assert_eq!(
        xxh3_of(&pkg_dir.join("frame_paced.fits")),
        xxh3_of(&dest.path().join("frame_paced.fits")),
        "pacing must not corrupt or truncate the payload"
    );

    // 3 MiB at 1 MB/s ⇒ ~3.1 s in theory. The floor is 2 s — two thirds of
    // that — so pacing jitter, chunk-size variance and the first (unpaced)
    // chunk can never flake it, while an unthrottled localhost transfer of the
    // same package (well under a second) still fails it decisively.
    //
    // Deliberately NO upper bound: wall-clock ceilings flake under CI and
    // parallel-test load. The completion direction is already pinned by
    // `throttle_intercept_zero_rate_transfer_completes`.
    assert!(
        elapsed >= Duration::from_secs(2),
        "paced 3 MiB transfer at 1 MB/s finished in {elapsed:?} — the throttle is not slowing the provider"
    );

    provider.shutdown().await;
    receiver.shutdown().await;
}

// ---------------------------------------------------------------------------
// D3 Task 1 — `fetch_collection_multi`: one collection, MANY providers.
//
// Three real `SharedIrohNode`s on localhost with the relay disabled. Providers
// A and B serve the SAME package directory — identical bytes ⇒ identical
// collection hash, because the import is a deterministic sorted-name walk — and
// puller C pulls it from both at once with `SplitStrategy::Split`, which issues
// one request per collection child across the full (re-shuffled) provider set.
// Per-provider telemetry is the oracle: it is the only thing that can say a
// child actually went to a given provider.
// ---------------------------------------------------------------------------

/// A [`ProviderTelemetrySink`] that records every event, plus the vec to inspect
/// after the fetch — the per-provider counterpart of [`recording_sink`].
fn recording_telemetry() -> (ProviderTelemetrySink, Arc<Mutex<Vec<ProviderEvent>>>) {
    let events: Arc<Mutex<Vec<ProviderEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_events = Arc::clone(&events);
    let sink: ProviderTelemetrySink = Arc::new(move |ev| {
        sink_events
            .lock()
            .expect("telemetry mutex poisoned")
            .push(ev);
    });
    (sink, events)
}

/// The set of provider ids the downloader reported trying.
///
/// **Sampling only, never an oracle.** Split-mode provider events are lossy on
/// iroh-blobs 0.103: `handle_download_split_impl` funnels each child's events
/// through its own 16-slot mpsc channel into a stream of receivers drained
/// SEQUENTIALLY (`into_stream(progress_rx).flat_map(into_stream)`), and when the
/// last child finishes it returns and drops every receiver it never reached. A
/// child's two events (Trying + PartComplete) fit its buffer without ever
/// blocking the child, so on fast localhost the children all finish long before
/// the drain walks their channels — instrumented runs observed 3 to 16 Trying
/// events for the same 15-child fetch. Asserting "both providers appear here"
/// therefore samples the few drained channels, which failed roughly 1 run in 8.
/// The sound oracle is the provider-side byte delta ([`sent_bytes`]); this stays
/// only as a pipe-sanity check that events flow at all.
fn tried_providers(events: &Arc<Mutex<Vec<ProviderEvent>>>) -> std::collections::HashSet<NodeId> {
    events
        .lock()
        .expect("telemetry mutex poisoned")
        .iter()
        .filter_map(|e| match e {
            ProviderEvent::Trying(id) => Some(*id),
            ProviderEvent::Failed(_) => None,
        })
        .collect()
}

/// Total bytes this node's endpoint has sent since bind (relay + direct).
///
/// Bracketing a fetch with two readings gives each PROVIDER's contribution
/// directly from iroh's own socket counters — ground truth that no progress
/// stream can lose.
fn sent_bytes(node: &Arc<SharedIrohNode>) -> u64 {
    let c = node.counters_snapshot_for_test();
    c.send_direct_bytes.saturating_add(c.send_relay_bytes)
}

/// A provider's send delta must clear this to count as "served real payload".
///
/// Sits between the two populations it has to separate, both measured by forcing
/// the regression it guards against (`SplitStrategy::Split` → `None`, so one
/// provider serves everything): the serving provider sent 1_430_494 B, the
/// non-serving one 15_789 B — phase-1 meta plus QUIC/TLS handshake and ACKs, not
/// the near-zero a naive reading would predict. 64 KiB leaves 4x headroom below
/// the noise floor and 21x below one served child, so the assertion has teeth
/// against a single-provider regression with no room to flake.
const SERVED_PAYLOAD_FLOOR: u64 = 64 * 1024;

async fn bind_disabled(dir: &Path) -> Arc<SharedIrohNode> {
    SharedIrohNode::bind(dir, RelayMode::Disabled)
        .await
        .expect("bind relay-disabled node")
}

/// A node bound with the relay disabled has no home relay, and its watch
/// starts at `None` too (live exchange presence beat, spec §4.2, task T4).
#[tokio::test]
async fn home_relay_url_and_watch_start_at_none_with_relay_disabled() {
    let dir = tempdir().unwrap();
    let node = bind_disabled(dir.path()).await;
    assert_eq!(node.home_relay_url(), None);
    let mut watch = node.home_relay_watch();
    assert_eq!(*watch.borrow(), None);
    // `shutdown` must abort the watcher task and drop its sender (T4 fix
    // round 1) rather than leaking it alongside the endpoint's own clone —
    // an outstanding receiver sees its watch end, same as any other watch
    // whose `Watchable` was dropped.
    node.shutdown().await;
    assert!(watch.changed().await.is_err());
}

async fn tag_present(store: &Store, name: &str) -> bool {
    store
        .tags()
        .get(name.as_bytes())
        .await
        .expect("tags().get")
        .is_some()
}

/// Write an N-payload package and return `(pkg_dir, announce)`.
///
/// The swarm tests need MANY collection children: `SplitStrategy::Split` issues
/// one request per child and re-shuffles the provider list for EACH of them, so
/// the chance that a given provider is never tried falls off as 2^-children.
/// With a dozen-plus payloads a "both providers were used" assertion is not a
/// coin flip.
fn build_many_file_package(
    src_root: &Path,
    prefix: &str,
    files: usize,
    size: usize,
) -> (PathBuf, PackageAnnounce) {
    std::fs::create_dir_all(src_root).unwrap();
    let mut records = Vec::with_capacity(files);
    for i in 0..files {
        let name = format!("frame_{i:02}.fits");
        let payload = src_root.join(&name);
        // Distinct, non-repeating content per file so a wrong-blob mixup fails
        // the hash check, not merely a size check.
        let bytes: Vec<u8> = (0..size).map(|j| ((j + i * 97) % 251) as u8).collect();
        std::fs::write(&payload, &bytes).unwrap();
        let byte_size = std::fs::metadata(&payload).unwrap().len();
        let xxh3 = package::xxh3_full_file(&payload).unwrap();
        records.push((
            payload,
            ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: format!("{prefix}-uuid-{i:02}"),
                origin_catalog_uuid: "catalog-uuid".to_string(),
                origin_device: "origin-device".to_string(),
                payload_kind: PayloadKind::RawFrame,
                rel_path: name,
                byte_size,
                xxh3,
                frame_meta: serde_json::json!({ "object": "M42" }),
                analysis: None,
                app_version: "test".to_string(),
                project: None,
            },
        ));
    }
    let pkg_dir = src_root.parent().unwrap().join(format!("pkg-{prefix}"));
    let announce = write_package(&pkg_dir, records).unwrap();
    (pkg_dir, announce)
}

/// Assert every payload of `pkg_dir` landed byte-identical under `dest_dir`.
fn assert_package_landed(pkg_dir: &Path, dest_dir: &Path, files: usize) {
    for i in 0..files {
        let name = format!("frame_{i:02}.fits");
        let landed = dest_dir.join(&name);
        assert!(
            landed.exists(),
            "swarm fetch must land every collection entry, {name} is missing"
        );
        assert_eq!(
            xxh3_of(&pkg_dir.join(&name)),
            xxh3_of(&landed),
            "{name} must land byte-identical (a mis-attributed child would differ)"
        );
    }
    assert!(
        dest_dir.join("manifest.ndjson").exists(),
        "the manifest entry must land alongside the payloads"
    );
}

/// Two providers serve the same package; the puller's Split fan-out uses BOTH.
#[tokio::test]
async fn multi_fetch_uses_both_providers() {
    let da = tempdir().unwrap();
    let db = tempdir().unwrap();
    let dc = tempdir().unwrap();
    let src = tempdir().unwrap();

    let a = bind_disabled(da.path()).await;
    let b = bind_disabled(db.path()).await;
    let c = bind_disabled(dc.path()).await;

    let a_out = a.handle(Role::Out);
    let b_out = b.handle(Role::Out);
    let c_recv = c.handle(Role::Recv);
    let a_info = a_out.start().await.unwrap();
    let b_info = b_out.start().await.unwrap();
    let c_info = c_recv.start().await.unwrap();

    // Relay-disabled endpoints have no discovery, so the puller needs BOTH
    // providers' addresses out of band (in production those are the holder dial
    // hints); the providers get the puller's address for symmetry with pairing.
    c.add_peer_ticket(&a_info.pairing_ticket).unwrap();
    c.add_peer_ticket(&b_info.pairing_ticket).unwrap();
    a.add_peer_ticket(&c_info.pairing_ticket).unwrap();
    b.add_peer_ticket(&c_info.pairing_ticket).unwrap();

    // ONE package dir, served by BOTH: the swarm's load-bearing invariant is
    // that identical bytes yield the identical collection hash on every holder.
    const FILES: usize = 14;
    let (pkg_dir, announce) =
        build_many_file_package(&src.path().join("src"), "swarm", FILES, 96 * 1024);
    a_out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    b_out.serve(&announce, &pkg_dir, None, None).await.unwrap();

    let root_a = a
        .resolve_served_hash_for_test(Role::Out, &announce.package_id)
        .expect("provider A recorded a served collection hash");
    let root_b = b
        .resolve_served_hash_for_test(Role::Out, &announce.package_id)
        .expect("provider B recorded a served collection hash");
    assert_eq!(
        root_a, root_b,
        "the same package bytes must import to the same collection hash on both \
         holders — without that there is no swarm to fetch from"
    );

    let (telemetry, seen) = recording_telemetry();
    let dest = tempdir().unwrap();

    // The oracle: each provider's OWN socket send counter, bracketed around the
    // fetch. Ground truth, and immune to the progress stream's lossiness.
    let a_before = sent_bytes(&a);
    let b_before = sent_bytes(&b);

    c_recv
        .fetch_collection_multi(
            vec![a_info.node_id, b_info.node_id],
            &root_a.to_string(),
            announce.byte_size,
            dest.path(),
            noop_fetch_sink(),
            telemetry,
        )
        .await
        .expect("the swarm fetch must complete");

    assert_package_landed(&pkg_dir, dest.path(), FILES);

    // With 15 children each independently re-shuffling a 2-provider list, the
    // chance that one provider wins zero first picks is 2^-15 — and a
    // single-provider regression puts the other's delta in the handshake range,
    // two orders of magnitude below the floor.
    let a_sent = sent_bytes(&a).saturating_sub(a_before);
    let b_sent = sent_bytes(&b).saturating_sub(b_before);
    assert!(
        a_sent > SERVED_PAYLOAD_FLOOR && b_sent > SERVED_PAYLOAD_FLOOR,
        "Split must spread the collection's children over BOTH providers — \
         a sent {a_sent} B, b sent {b_sent} B (floor {SERVED_PAYLOAD_FLOOR} B)"
    );

    // Sanity only: the telemetry pipe is wired and delivers SOMETHING. It cannot
    // be asserted per-provider — see `tried_providers`.
    assert!(
        !tried_providers(&seen).is_empty(),
        "the provider telemetry sink must receive at least one event"
    );

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

/// A provider killed mid-transfer costs its in-flight children a provider
/// switch, not the fetch: the survivor finishes the package.
#[tokio::test]
async fn multi_fetch_survives_a_provider_dying_mid_transfer() {
    let da = tempdir().unwrap();
    let db = tempdir().unwrap();
    let dc = tempdir().unwrap();
    let src = tempdir().unwrap();

    let a = bind_disabled(da.path()).await;
    let b = bind_disabled(db.path()).await;
    let c = bind_disabled(dc.path()).await;

    let a_out = a.handle(Role::Out);
    let b_out = b.handle(Role::Out);
    let c_recv = c.handle(Role::Recv);
    let a_info = a_out.start().await.unwrap();
    let b_info = b_out.start().await.unwrap();
    let c_info = c_recv.start().await.unwrap();

    c.add_peer_ticket(&a_info.pairing_ticket).unwrap();
    c.add_peer_ticket(&b_info.pairing_ticket).unwrap();
    a.add_peer_ticket(&c_info.pairing_ticket).unwrap();
    b.add_peer_ticket(&c_info.pairing_ticket).unwrap();

    const FILES: usize = 16;
    let (pkg_dir, announce) =
        build_many_file_package(&src.path().join("src"), "dying", FILES, 512 * 1024);
    a_out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    b_out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    let root = a
        .resolve_served_hash_for_test(Role::Out, &announce.package_id)
        .expect("provider A recorded a served collection hash");

    // Sizing note: 8 MiB paced at 2 MB/s PER PROVIDER (~4 MB/s combined) puts the
    // transfer in the multi-second range, so the 150 ms kill below lands solidly
    // mid-fetch on any machine. Pacing rather than a huge package keeps the test
    // I/O small; the assertion right before the kill is what makes "mid-transfer"
    // a fact rather than a hope — if it ever fails, lower the rate.
    a.set_upload_limit(2_000_000);
    b.set_upload_limit(2_000_000);

    let (telemetry, seen) = recording_telemetry();
    let dest = tempdir().unwrap();
    let a_before = sent_bytes(&a);
    let b_before = sent_bytes(&b);
    let fetch_done = Arc::new(AtomicBool::new(false));
    let task = {
        let c_recv = Arc::clone(&c_recv);
        let done = Arc::clone(&fetch_done);
        let dest_path = dest.path().to_path_buf();
        let hash = root.to_string();
        let providers = vec![a_info.node_id, b_info.node_id];
        let byte_size = announce.byte_size;
        tokio::spawn(async move {
            let res = c_recv
                .fetch_collection_multi(
                    providers,
                    &hash,
                    byte_size,
                    &dest_path,
                    noop_fetch_sink(),
                    telemetry,
                )
                .await;
            done.store(true, Ordering::SeqCst);
            res
        })
    };

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !fetch_done.load(Ordering::SeqCst),
        "the fetch finished before the provider could be killed — the kill would \
         prove nothing; lower the upload limit or grow the package"
    );
    // Read A's contribution BEFORE killing it: a shut-down endpoint's counters
    // are not something to lean on, and this is the number that says the victim
    // was genuinely serving when it died (17 children each re-shuffle a
    // 2-provider list, so A wins ~half the first picks and, paced at 2 MB/s, has
    // pushed ~300 KB by now).
    let a_sent = sent_bytes(&a).saturating_sub(a_before);
    a.shutdown().await;

    let res = tokio::time::timeout(Duration::from_secs(120), task)
        .await
        .expect("the fetch must not hang after a provider dies")
        .expect("fetch task panicked");
    res.expect("the surviving provider must finish the package");

    assert_package_landed(&pkg_dir, dest.path(), FILES);

    // Both halves of the story, on provider-side byte counters rather than the
    // lossy progress stream: the victim was really serving when it died, and the
    // survivor really carried the package home.
    let b_sent = sent_bytes(&b).saturating_sub(b_before);
    assert!(
        a_sent > SERVED_PAYLOAD_FLOOR,
        "the provider we killed must have been serving payload when it died — \
         it sent only {a_sent} B (floor {SERVED_PAYLOAD_FLOOR} B)"
    );
    assert!(
        b_sent > SERVED_PAYLOAD_FLOOR,
        "the surviving provider must have served the rest of the package — \
         it sent only {b_sent} B (floor {SERVED_PAYLOAD_FLOOR} B)"
    );

    // Sanity only, never per-provider — see `tried_providers`.
    assert!(
        !tried_providers(&seen).is_empty(),
        "the provider telemetry sink must receive at least one event"
    );

    b.shutdown().await;
    c.shutdown().await;
}

/// Every provider dead ⇒ a bounded, clean failure — and no in-flight GC tag,
/// because phase 1 never landed the root that tag would protect.
#[tokio::test]
async fn multi_fetch_with_all_dead_providers_fails_cleanly() {
    let dc = tempdir().unwrap();
    let c = bind_disabled(dc.path()).await;
    let c_recv = c.handle(Role::Recv);
    c_recv.start().await.unwrap();

    // Well-formed ids nobody ever bound: the puller holds no address for either,
    // so both fail at the dial.
    let dead_a: NodeId = *iroh::SecretKey::generate().public().as_bytes();
    let dead_b: NodeId = *iroh::SecretKey::generate().public().as_bytes();
    let root = Hash::new(b"a collection nobody serves");

    let (telemetry, _seen) = recording_telemetry();
    let dest = tempdir().unwrap();
    let dest_dir = dest.path().join("landing");
    let res = tokio::time::timeout(
        Duration::from_secs(60),
        c_recv.fetch_collection_multi(
            vec![dead_a, dead_b],
            &root.to_string(),
            1024,
            &dest_dir,
            noop_fetch_sink(),
            telemetry,
        ),
    )
    .await
    .expect("an all-dead provider set must fail, never hang");
    assert!(
        res.is_err(),
        "a fetch whose every provider is unreachable must return Err"
    );

    // The in-flight GC tag is set only AFTER phase 1 lands the root hash-seq —
    // the very thing that failed here — so nothing was tagged. (Had phase 1
    // succeeded and phase 2 failed, the tag would deliberately be RETAINED so a
    // retry resumes from the verified partial bytes; that asymmetry is the
    // scalar fetch's documented contract and this sibling keeps it.)
    assert!(
        !tag_present(c.store(), &format!("in-flight/recv/pkg/{}", root.to_hex())).await,
        "a fetch that never got past phase 1 must leave no in-flight tag behind"
    );
    assert!(
        !dest_dir.exists(),
        "a failed fetch must not even create the destination directory"
    );

    c.shutdown().await;
}

// ---------------------------------------------------------------------------
// A2a Task 7 — `SwarmFetchMode::Assigned`: our own assignment loop.
//
// The same three-node localhost harness as the D3 tests above, driven through
// `fetch_collection_multi_tuned_for_test` so the test can shorten the progress
// deadline and read the `AssignmentReport` production throws away.
// ---------------------------------------------------------------------------

/// A [`StartInfo`]-shaped node id as the assignment report keys it.
fn endpoint_id(id: NodeId) -> EndpointId {
    EndpointId::from_bytes(&id).expect("a node id from StartInfo is a valid endpoint id")
}

fn provider_stats(report: &AssignmentReport, id: NodeId) -> ProviderStats {
    report
        .per_provider
        .get(&endpoint_id(id))
        .cloned()
        .expect("every provider handed to the loop appears in its report")
}

/// A provider that ACCEPTS and then trickles must not pin the fetch: the
/// assignment loop's stall ceiling reassigns its children and the fetch
/// finishes on the healthy provider inside a bounded time.
///
/// **On the trickle rate.** B is paced at 1 KB/s — two orders of magnitude
/// below the *product's* 100 KB/s floor, which is a Settings validation rule
/// (`api::sync::validate_upload_limit`), not a transport clamp. That is
/// deliberate and it is the only rate that tests what this test is for: the
/// provider emits one progress item per ~16 KiB chunk, so at the product floor
/// the gap between two items is ~164 ms and NO stall ceiling short of ~200 ms
/// could ever fire — and a 200 ms ceiling would be tripped by a loaded CI
/// runner on a perfectly healthy provider. At 1 KB/s a 16 KiB chunk needs 16 s,
/// so "no growth for 1500 ms" is unambiguous: B is the useless-but-alive peer
/// of D4 §1(б), not a slow one.
#[tokio::test]
async fn assigned_fetch_reassigns_a_trickling_provider() {
    let da = tempdir().unwrap();
    let db = tempdir().unwrap();
    let dc = tempdir().unwrap();
    let src = tempdir().unwrap();

    let a = bind_disabled(da.path()).await;
    let b = bind_disabled(db.path()).await;
    let c = bind_disabled(dc.path()).await;

    let a_out = a.handle(Role::Out);
    let b_out = b.handle(Role::Out);
    let c_recv = c.handle(Role::Recv);
    let a_info = a_out.start().await.unwrap();
    let b_info = b_out.start().await.unwrap();
    let c_info = c_recv.start().await.unwrap();

    c.add_peer_ticket(&a_info.pairing_ticket).unwrap();
    c.add_peer_ticket(&b_info.pairing_ticket).unwrap();
    a.add_peer_ticket(&c_info.pairing_ticket).unwrap();
    b.add_peer_ticket(&c_info.pairing_ticket).unwrap();

    const FILES: usize = 8;
    const FILE_SIZE: usize = 256 * 1024;
    let (pkg_dir, announce) =
        build_many_file_package(&src.path().join("src"), "trickle", FILES, FILE_SIZE);
    a_out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    b_out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    let root = a
        .resolve_served_hash_for_test(Role::Out, &announce.package_id)
        .expect("provider A recorded a served collection hash");

    // A unthrottled, B trickling — see the doc comment for the arithmetic.
    b.set_upload_limit(1_000);

    let (telemetry, seen) = recording_telemetry();
    let dest = tempdir().unwrap();
    let started = Instant::now();
    let report = tokio::time::timeout(
        // Was 30 s; raised to 120 s (Wave 1 final review item 3) for the same
        // CI-margin reason as the other assignment-loop tests below — a
        // 4-CPU runner saturated by the rest of the workspace has left this
        // test past its old deadline before.
        Duration::from_secs(120),
        c.fetch_collection_multi_tuned_for_test(
            Role::Recv,
            vec![a_info.node_id, b_info.node_id],
            &root.to_string(),
            announce.byte_size,
            dest.path(),
            noop_fetch_sink(),
            telemetry,
            SwarmFetchMode::Assigned,
            Duration::from_millis(1500),
            true,
        ),
    )
    .await
    .expect("a trickling provider must not pin the fetch past the stall ceiling")
    .expect("the healthy provider must finish the package")
    .expect("the assigned loop always reports");
    let elapsed = started.elapsed();

    assert_package_landed(&pkg_dir, dest.path(), FILES);

    let a_stats = provider_stats(&report, a_info.node_id);
    let b_stats = provider_stats(&report, b_info.node_id);

    // A carried the bulk. B cannot have completed a single child: one 256 KiB
    // child needs 262 s at 1 KB/s and the ceiling cuts it at 1.5 s, so the
    // remaining 8 frames + the manifest are all A's.
    assert!(
        a_stats.bytes > 6 * FILE_SIZE as u64,
        "the healthy provider must have served the bulk of the package — \
         a delivered {} B of {} B, b delivered {} B (elapsed {elapsed:?})",
        a_stats.bytes,
        (FILES * FILE_SIZE) as u64,
        b_stats.bytes
    );

    // And the trickling one was stalled out rather than waited on.
    assert!(
        b_stats.failures >= 1,
        "the trickling provider must have been failed out by the stall ceiling, \
         not waited for — its stats are {b_stats:?}"
    );
    assert!(
        report.stalls >= 1,
        "the failure must be attributed to the progress deadline (a stall), not \
         to an error the stock loop would also have caught — report {report:?}"
    );

    // The loop's own telemetry is not lossy the way the Split stream is: every
    // attempt it made is reported, so the trickling provider IS visible here.
    assert!(
        tried_providers(&seen).contains(&b_info.node_id),
        "the assignment loop must report the attempts it made on the trickling provider"
    );

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

/// Every provider dead (connection refused) ⇒ the loop exhausts its backoff
/// ladder and returns Err inside a bounded time instead of spinning — and it
/// keeps the in-flight GC tag, because the verified partial bytes are what
/// makes the next attempt cheap.
///
/// **On the bound.** The brief says "≈ 32 s", which is the ladder's own
/// `0.5 + 1 + 2 + 4 + 8 + 16` of WAITING and omits the dial between each rung:
/// the connection pool's `connect_timeout` is 1 s (iroh-util 0.6.0's default)
/// and there are seven or so of them, so the honest figure is ≈ 40 s. Measured
/// at 38-41 s on this machine; the 60 s bound below is that plus room for a
/// loaded runner, not the brief's number plus a guess.
///
/// **Why phase 1 is primed first.** The in-flight tag is set only after phase 1
/// lands the root hash-seq (`multi_fetch_with_all_dead_providers_fails_cleanly`
/// pins the other half of that contract: no phase 1, no tag). To reach the
/// assignment loop at all with a dead provider set, the puller must already
/// hold the root and the collection meta — so the test fetches the manifest
/// from A while A is alive, which is exactly phase 1's request, and only then
/// kills A. The stock `execute_get` short-circuits on `local.is_complete()`
/// before it awaits the dial, so phase 1 then succeeds against a corpse.
#[tokio::test]
async fn assigned_fetch_fails_fast_when_every_provider_is_dead() {
    let da = tempdir().unwrap();
    let dc = tempdir().unwrap();
    let src = tempdir().unwrap();

    let a = bind_disabled(da.path()).await;
    let c = bind_disabled(dc.path()).await;

    let a_out = a.handle(Role::Out);
    let c_recv = c.handle(Role::Recv);
    let a_info = a_out.start().await.unwrap();
    let c_info = c_recv.start().await.unwrap();
    c.add_peer_ticket(&a_info.pairing_ticket).unwrap();
    a.add_peer_ticket(&c_info.pairing_ticket).unwrap();

    const FILES: usize = 3;
    let (pkg_dir, announce) =
        build_many_file_package(&src.path().join("src"), "dead", FILES, 64 * 1024);
    a_out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    let root = a
        .resolve_served_hash_for_test(Role::Out, &announce.package_id)
        .expect("provider A recorded a served collection hash");

    // Prime phase 1 (root hash-seq + collection meta) while A still answers.
    // `write_package`'s announce carries a placeholder root hash, so point this
    // one at the real collection hash.
    let mut real = announce.clone();
    real.root_hash = root.to_string();
    let manifest_dir = tempdir().unwrap();
    c_recv
        .fetch_manifest(a_info.node_id, &real, manifest_dir.path())
        .await
        .expect("the manifest fetch primes the root hash-seq and the collection meta");

    a.shutdown().await;

    let (telemetry, _seen) = recording_telemetry();
    let dest = tempdir().unwrap();
    let started = Instant::now();
    let res = tokio::time::timeout(
        // Was 60 s; raised to 120 s (Wave 1 final review item 3) for CI
        // margin — the ladder's own ~40 s measured bound plus dials already
        // ate most of 60 s on a quiet machine, leaving nothing for a
        // saturated 4-CPU runner.
        Duration::from_secs(120),
        c.fetch_collection_multi_tuned_for_test(
            Role::Recv,
            vec![a_info.node_id],
            &root.to_string(),
            announce.byte_size,
            dest.path(),
            noop_fetch_sink(),
            telemetry,
            SwarmFetchMode::Assigned,
            // Production's ceiling: nothing here ever connects, so the ladder,
            // not the stall watchdog, is what bounds this test.
            super::assign::STALL_HARD_LIMIT,
            true,
        ),
    )
    .await
    .expect("a dead swarm must fail inside the backoff ladder, never hang");
    let elapsed = started.elapsed();

    assert!(
        res.is_err(),
        "a fetch whose every provider is unreachable must return Err, got {res:?}"
    );
    // The ladder is 500 ms doubling six times ≈ 31.5 s of waiting plus the
    // dials between the rungs. A lower bound is the honest assertion here: it
    // fails if the loop ever gives up on the first dial (no ladder at all),
    // and the 120 s timeout above fails if it spins.
    assert!(
        elapsed >= Duration::from_secs(5),
        "the loop must walk its backoff ladder before giving up, not fail on the \
         first dial — it gave up after {elapsed:?}"
    );

    // The partial bytes phase 1 landed stay GC-protected: a later attempt
    // against a different holder set resumes from them. Same contract as the
    // stock path, which is the point of keeping the tag out of the phase-2 arm.
    assert!(
        tag_present(c.store(), &format!("in-flight/recv/pkg/{}", root.to_hex())).await,
        "a failed assignment loop must KEEP the in-flight tag so the verified \
         partial bytes survive for the next attempt"
    );

    c.shutdown().await;
}

/// The mode flag: Stock and Assigned both land the identical package, and the
/// Assigned report accounts every byte the providers' socket counters saw.
///
/// The report is our own bookkeeping; `sent_bytes` is iroh's socket counter on
/// the other side of the wire. Bracketing one against the other is the only
/// check that the loop's per-provider attribution is real rather than
/// self-consistent — and it is what A4's ranking will be built on.
///
/// **Hedging is forced OFF here, and it has to be.** Once a hedge can fire, a
/// provider's `bytes` and its socket egress stop being the same quantity: a
/// cancelled loser puts bytes on the wire that its `Stats` never report, and a
/// hedge adds a second assignment to a provider that the child-count no longer
/// counts. Per-provider bytes are an attribution oracle, not an egress oracle,
/// the moment hedging is on — so the reconciliation is pinned in the one mode
/// where the two do line up, and the hedge path is measured by its own tests.
#[tokio::test]
async fn assigned_fetch_report_matches_provider_send_counters() {
    let da = tempdir().unwrap();
    let db = tempdir().unwrap();
    let dc = tempdir().unwrap();
    let dd = tempdir().unwrap();
    let src = tempdir().unwrap();

    let a = bind_disabled(da.path()).await;
    let b = bind_disabled(db.path()).await;
    let c = bind_disabled(dc.path()).await;
    let d = bind_disabled(dd.path()).await;

    let a_out = a.handle(Role::Out);
    let b_out = b.handle(Role::Out);
    let c_recv = c.handle(Role::Recv);
    let d_recv = d.handle(Role::Recv);
    let a_info = a_out.start().await.unwrap();
    let b_info = b_out.start().await.unwrap();
    let c_info = c_recv.start().await.unwrap();
    let d_info = d_recv.start().await.unwrap();

    for puller in [&c, &d] {
        puller.add_peer_ticket(&a_info.pairing_ticket).unwrap();
        puller.add_peer_ticket(&b_info.pairing_ticket).unwrap();
    }
    for provider in [&a, &b] {
        provider.add_peer_ticket(&c_info.pairing_ticket).unwrap();
        provider.add_peer_ticket(&d_info.pairing_ticket).unwrap();
    }

    // 256 KiB per entry, not the D3 tests' 96 KiB: the byte-reconciliation
    // below compares payload against total socket egress, and a provider's
    // fixed handshake + phase-1 cost (~16 KB, measured in `SERVED_PAYLOAD_FLOOR`)
    // is 16 % of a 96 KiB child but under 6 % of a 256 KiB one.
    const FILES: usize = 14;
    let (pkg_dir, announce) =
        build_many_file_package(&src.path().join("src"), "report", FILES, 256 * 1024);
    a_out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    b_out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    let root = a
        .resolve_served_hash_for_test(Role::Out, &announce.package_id)
        .expect("provider A recorded a served collection hash");

    // Arm 1: the stock Split fan-out, into puller C.
    let stock_dest = tempdir().unwrap();
    let (stock_telemetry, _) = recording_telemetry();
    // 120 s harness timeout (Wave 1 final review item 3): this test had none,
    // so a wedge here hung the whole CI job instead of failing it.
    let stock_report = tokio::time::timeout(
        Duration::from_secs(120),
        c.fetch_collection_multi_tuned_for_test(
            Role::Recv,
            vec![a_info.node_id, b_info.node_id],
            &root.to_string(),
            announce.byte_size,
            stock_dest.path(),
            noop_fetch_sink(),
            stock_telemetry,
            SwarmFetchMode::Stock,
            super::assign::STALL_HARD_LIMIT,
            false,
        ),
    )
    .await
    .expect("arm 1 must not hang")
    .expect("the stock fan-out must still complete");
    assert!(
        stock_report.is_none(),
        "the stock path runs upstream's loop and has no per-provider report to give"
    );
    assert_package_landed(&pkg_dir, stock_dest.path(), FILES);

    // Arm 2: the assignment loop, into a FRESH puller D so nothing is already
    // local — bracketed by both providers' socket counters.
    let dest = tempdir().unwrap();
    let (telemetry, _) = recording_telemetry();
    let a_before = sent_bytes(&a);
    let b_before = sent_bytes(&b);
    // Same 120 s harness timeout as arm 1 above, same reason.
    let report = tokio::time::timeout(
        Duration::from_secs(120),
        d.fetch_collection_multi_tuned_for_test(
            Role::Recv,
            vec![a_info.node_id, b_info.node_id],
            &root.to_string(),
            announce.byte_size,
            dest.path(),
            noop_fetch_sink(),
            telemetry,
            SwarmFetchMode::Assigned,
            super::assign::STALL_HARD_LIMIT,
            // See the doc: with hedging on, `bytes` and egress are different
            // quantities and this reconciliation would be asserting nonsense.
            false,
        ),
    )
    .await
    .expect("arm 2 must not hang")
    .expect("the assigned loop must complete")
    .expect("the assigned loop always reports");
    let a_sent = sent_bytes(&a).saturating_sub(a_before);
    let b_sent = sent_bytes(&b).saturating_sub(b_before);

    // Both modes land the identical package: each is byte-identical to the
    // source, so they are byte-identical to each other.
    assert_package_landed(&pkg_dir, dest.path(), FILES);

    for (name, id, sent) in [("a", a_info.node_id, a_sent), ("b", b_info.node_id, b_sent)] {
        let stats = provider_stats(&report, id);
        assert!(
            stats.bytes <= sent,
            "provider {name} cannot have delivered more payload than its socket \
             sent: report {} B vs sent {sent} B",
            stats.bytes
        );
        assert!(
            stats.bytes * 10 >= sent * 9,
            "provider {name}'s reported payload must account for its egress \
             bar framing: report {} B vs sent {sent} B ({:.1} %)",
            stats.bytes,
            stats.bytes as f64 * 100.0 / sent.max(1) as f64
        );
    }

    // Every collection child is attributed to exactly one provider. The
    // collection carries FILES payload entries PLUS `manifest.ndjson`, which is
    // an ordinary entry and an ordinary child — the hash-seq's child 0 (the
    // collection meta) is phase 1's and is not assigned here.
    assert_eq!(
        report.total_children() as usize,
        FILES + 1,
        "every child, manifest included, must be accounted for exactly once: {report:?}"
    );

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
    d.shutdown().await;
}

/// The Assigned path's aggregate progress is a well-formed series.
///
/// It is summed from the per-file `store.observe()` observers (D4 T7 — the get
/// streams' own `Progress` never leaves the assignment loop) and emitted by a
/// ticker that is aborted AND JOINED before the terminal 100 % event, so the
/// series can only go forwards and can only end at the announced total. The
/// providers are paced so the fetch outlives a few ticker intervals; without
/// that, a localhost fetch would emit the terminal event alone and the
/// monotonicity claim would be vacuous.
#[tokio::test]
async fn assigned_fetch_batch_progress_is_monotonic_and_ends_at_the_total() {
    let da = tempdir().unwrap();
    let db = tempdir().unwrap();
    let dc = tempdir().unwrap();
    let src = tempdir().unwrap();

    let a = bind_disabled(da.path()).await;
    let b = bind_disabled(db.path()).await;
    let c = bind_disabled(dc.path()).await;

    let a_out = a.handle(Role::Out);
    let b_out = b.handle(Role::Out);
    let c_recv = c.handle(Role::Recv);
    let a_info = a_out.start().await.unwrap();
    let b_info = b_out.start().await.unwrap();
    let c_info = c_recv.start().await.unwrap();
    c.add_peer_ticket(&a_info.pairing_ticket).unwrap();
    c.add_peer_ticket(&b_info.pairing_ticket).unwrap();
    a.add_peer_ticket(&c_info.pairing_ticket).unwrap();
    b.add_peer_ticket(&c_info.pairing_ticket).unwrap();

    const FILES: usize = 12;
    let (pkg_dir, announce) =
        build_many_file_package(&src.path().join("src"), "batch", FILES, 256 * 1024);
    a_out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    b_out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    let root = a
        .resolve_served_hash_for_test(Role::Out, &announce.package_id)
        .expect("provider A recorded a served collection hash");

    // ~3 MiB at 2 MB/s per provider ⇒ the fetch spans several 300 ms ticks.
    a.set_upload_limit(2_000_000);
    b.set_upload_limit(2_000_000);

    let (sink, events) = recording_sink();
    let (telemetry, _) = recording_telemetry();
    let dest = tempdir().unwrap();
    // 120 s harness timeout (Wave 1 final review item 3): this test had none.
    tokio::time::timeout(
        Duration::from_secs(120),
        c.fetch_collection_multi_tuned_for_test(
            Role::Recv,
            vec![a_info.node_id, b_info.node_id],
            &root.to_string(),
            announce.byte_size,
            dest.path(),
            sink,
            telemetry,
            SwarmFetchMode::Assigned,
            super::assign::STALL_HARD_LIMIT,
            true,
        ),
    )
    .await
    .expect("the fetch must not hang")
    .expect("the fetch must complete")
    .expect("the assigned loop always reports");
    assert_package_landed(&pkg_dir, dest.path(), FILES);

    let batches: Vec<(u64, u64)> = events
        .lock()
        .expect("sink mutex poisoned")
        .iter()
        .filter_map(|e| match e {
            FetchEvent::Batch {
                bytes_done,
                bytes_total,
            } => Some((*bytes_done, *bytes_total)),
            _ => None,
        })
        .collect();

    assert!(
        !batches.is_empty(),
        "the Assigned path must emit aggregate progress, not only per-file events"
    );
    for pair in batches.windows(2) {
        assert!(
            pair[1].0 >= pair[0].0,
            "the batch series must never go backwards: {} then {} in {batches:?}",
            pair[0].0,
            pair[1].0
        );
    }
    assert!(
        batches.iter().all(|(done, _)| *done <= announce.byte_size),
        "no tick may exceed the announced total: {batches:?}"
    );
    assert!(
        batches
            .iter()
            .all(|(_, total)| *total == announce.byte_size),
        "every tick must quote the same announced total: {batches:?}"
    );
    assert_eq!(
        batches.last().expect("non-empty").0,
        announce.byte_size,
        "the LAST event must be the terminal 100 % one — a ticker tick landing \
         after it would mean the abort was never joined: {batches:?}"
    );

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

// ---------------------------------------------------------------------------
// A2b Task 8 — hedging (D4 §4.5 T4).
//
// Every one of these uses THREE providers, two healthy and one useless. That is
// not decoration: the hedge budget's cap is 5 % of the COLLECTION, and tokens
// only refill as children complete, so a swarm split evenly between one fast
// and one slow provider parks `tokens` at exactly `cap / 2` — the strict `>`
// in the half rule then refuses every hedge, for arithmetic reasons rather than
// policy ones. A third provider puts two thirds of the children on healthy
// peers and takes the decision off that knife edge. It is reported as a design
// observation, not worked around in the code.
// ---------------------------------------------------------------------------

/// Bind three nodes, pair them, serve `files × size` from the two healthy ones
/// and return everything the hedge tests need.
struct HedgeRig {
    fast_a: Arc<SharedIrohNode>,
    fast_b: Arc<SharedIrohNode>,
    slow: Arc<SharedIrohNode>,
    puller: Arc<SharedIrohNode>,
    providers: Vec<NodeId>,
    slow_id: NodeId,
    root: Hash,
    announce: PackageAnnounce,
    pkg_dir: PathBuf,
    _dirs: Vec<tempfile::TempDir>,
}

async fn hedge_rig(prefix: &str, files: usize, size: usize, slow_rate: u64) -> HedgeRig {
    let da = tempdir().unwrap();
    let db = tempdir().unwrap();
    let ds = tempdir().unwrap();
    let dp = tempdir().unwrap();
    let src = tempdir().unwrap();

    let a = bind_disabled(da.path()).await;
    let b = bind_disabled(db.path()).await;
    let s = bind_disabled(ds.path()).await;
    let p = bind_disabled(dp.path()).await;

    let a_out = a.handle(Role::Out);
    let b_out = b.handle(Role::Out);
    let s_out = s.handle(Role::Out);
    let p_recv = p.handle(Role::Recv);
    let a_info = a_out.start().await.unwrap();
    let b_info = b_out.start().await.unwrap();
    let s_info = s_out.start().await.unwrap();
    let p_info = p_recv.start().await.unwrap();

    for t in [
        &a_info.pairing_ticket,
        &b_info.pairing_ticket,
        &s_info.pairing_ticket,
    ] {
        p.add_peer_ticket(t).unwrap();
    }
    for provider in [&a, &b, &s] {
        provider.add_peer_ticket(&p_info.pairing_ticket).unwrap();
    }

    let (pkg_dir, announce) = build_many_file_package(&src.path().join("src"), prefix, files, size);
    for out in [&a_out, &b_out, &s_out] {
        out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    }
    let root = a
        .resolve_served_hash_for_test(Role::Out, &announce.package_id)
        .expect("provider A recorded a served collection hash");

    // The useless-but-alive peer of D4 §1(б): it accepts, and then dribbles.
    // Below the product's 100 KB/s floor on purpose — that floor is a Settings
    // validation rule (`api::sync::validate_upload_limit`), not a transport
    // clamp, and a peer that is merely slow cannot test a deadline.
    s.set_upload_limit(slow_rate);

    HedgeRig {
        providers: vec![a_info.node_id, b_info.node_id, s_info.node_id],
        slow_id: s_info.node_id,
        fast_a: a,
        fast_b: b,
        slow: s,
        puller: p,
        root,
        announce,
        pkg_dir,
        _dirs: vec![da, db, ds, dp, src],
    }
}

impl HedgeRig {
    async fn shutdown(self) {
        self.fast_a.shutdown().await;
        self.fast_b.shutdown().await;
        self.slow.shutdown().await;
        self.puller.shutdown().await;
    }
}

/// A provider that goes slow MID-TRANSFER triggers a hedge well before the
/// stall ceiling, and the fetch finishes on the healthy peers.
///
/// **This is D4 §1(б)'s scenario, and it is the only one the trigger can see.**
/// The rule is `max(p95, HEDGE_EXPECTED_MULTIPLIER × expected)` with
/// `expected = missing / ewma_goodput(THIS provider)`, so a peer that is
/// UNIFORMLY slow is never late by its own standard — an instrumented run of a
/// constantly-throttled peer refused 388 hedges on exactly that branch, because
/// the peer had completed the tiny `manifest.ndjson` child and thereby measured
/// itself at its own crawl. The design's own example is the laptop that "woke
/// up but is sitting on a mobile uplink": fast history, slow now. So this test
/// builds that — every provider paced the same to begin with, and the victim
/// dropped to 1 KB/s once the transfer is under way, which makes `expected`
/// tiny against what it has already proved it can do and the hedge immediate.
///
/// The discriminator is `report.stalls == 0`: the 60 s ceiling never fired, so
/// whatever rescued the children was the hedge.
///
/// Timing margin (reviewer item 10, noted not tightened): the assertion is 45 s
/// against a measured ~8 s, under a 120 s harness timeout, and the ceiling it
/// must beat is 60 s. The claim is therefore still "hedging got there before
/// the deadline could" — reinforced by `stalls == 0`, which is the assertion
/// that does not depend on a clock at all — while leaving room for a runner
/// under load, where a 30 s bound failed at load 20+.
#[tokio::test]
async fn hedge_fires_before_the_stall_ceiling_on_a_slow_provider() {
    // FORTY children, not twelve, and that is the budget's arithmetic talking:
    // the cap is 5 % of the COLLECTION while one hedge costs half of what a
    // child still needs, so a package must hold about `10 x the number of
    // children stranded at once` before every one of them can be hedged. At
    // twelve the bucket refused one of the stranded four — and a refused child
    // is stuck for good here, because a peer dribbling 1 KB/s still delivers a
    // 16 KiB chunk every 16 s and so never trips the 60 s progress deadline
    // either. Forty children of 256 KiB give the bucket room for two hedges at
    // once, and a winning hedge refunds in full, so the stranded set clears in
    // a couple of seconds.
    const FILES: usize = 40;
    const FILE_SIZE: usize = 256 * 1024;
    // 400 KB/s each to start. The rate is deliberately low: the collapse fires
    // on a 600 ms timer, so the transfer must still be well under way at that
    // point NO MATTER how contended the machine is — `cargo test` runs this
    // beside ~140 other tests, several of which also bind iroh nodes, and at
    // 1.5 MB/s the fetch could outrun its own collapse there and strand
    // nobody. ~10.5 MB over an aggregate ~1.2 MB/s leaves ~93 % to go at 600 ms.
    // The victim still measures a healthy goodput for itself first, which is
    // what makes it "fast history, slow now" rather than uniformly slow.
    let rig = hedge_rig("hedge-slow", FILES, FILE_SIZE, 400_000).await;
    rig.fast_a.set_upload_limit(400_000);
    rig.fast_b.set_upload_limit(400_000);

    let (telemetry, _) = recording_telemetry();
    let dest = tempdir().unwrap();
    let started = Instant::now();
    // The uplink collapses mid-transfer. The COLLAPSE rides a timer task, not
    // the fetch: awaiting the fetch inline means a timeout DROPS it, which
    // cancels it cleanly, where a spawned fetch would keep running under a
    // panicking test and can block the runtime's own shutdown.
    let collapse = {
        let slow = Arc::clone(&rig.slow);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            slow.set_upload_limit(1_000);
        })
    };
    let report = tokio::time::timeout(
        // 120 s of patience for a 45 s claim: the harness bound must not be
        // what fails on a loaded runner, or a timing-margin failure reads as a
        // hedging failure.
        Duration::from_secs(120),
        rig.puller.fetch_collection_multi_tuned_for_test(
            Role::Recv,
            rig.providers.clone(),
            &rig.root.to_string(),
            rig.announce.byte_size,
            dest.path(),
            noop_fetch_sink(),
            telemetry,
            SwarmFetchMode::Assigned,
            // The brief's ceiling: long enough that it cannot be what rescues
            // the fetch.
            Duration::from_secs(60),
            true,
        ),
    )
    .await
    .expect("a hedged fetch must not wait out the stall ceiling")
    .expect("the fetch must complete")
    .expect("the assigned loop always reports");
    let elapsed = started.elapsed();
    collapse.await.ok();

    assert_package_landed(&rig.pkg_dir, dest.path(), FILES);
    assert!(
        elapsed < Duration::from_secs(45),
        "hedging must finish the package before the 60 s ceiling could — it took \
         {elapsed:?} (report {report:?})"
    );
    assert!(
        report.hedges >= 1,
        "at least one assignment must have been hedged: {report:?}"
    );
    assert_eq!(
        report.stalls, 0,
        "the 60 s ceiling must not be what rescued this fetch — hedging must \
         have got there first: {report:?}"
    );

    rig.shutdown().await;
}

/// The loser is REALLY cancelled: the slow provider's own socket send counter
/// stays under one child's worth, where an uncancelled primary would have gone
/// on streaming every child it was assigned.
///
/// The oracle is the loser's own socket counter. The bound is four children
/// plus the handshake floor rather than the brief's one — see the body for the
/// property of the assignment loop that makes one wrong — and the body also
/// records a measured reason not to reach for a ratio against `rate x time`.
///
/// Timing margin (reviewer item 10, noted not tightened): the absolute bound is
/// 1 114 112 B, measured at ~200 KB with the cancel working and 3 121 080 B
/// with it mutated out (both at 50 KB/s). The ratio bound beside it is 3x and
/// does not move with the clock at all.
#[tokio::test]
async fn hedge_cancels_the_loser_and_bounds_duplicate_bytes() {
    // FORTY 256 KiB children, like the storm tests, and for the same budget
    // arithmetic: the cap is 5 % of the collection while one hedge costs half
    // of what a child still needs, so a package must hold about `10 x the
    // number of children stranded at once` before all of them can be hedged.
    // At twelve 1 MiB children the bucket afforded ONE hedge at a time, the
    // rest of the slow peer's children queued behind it, and at 5 KB/s a queued
    // 1 MiB child needs 210 s — the fetch blew its own harness timeout
    // (observed twice, at 128 s). Forty small children give the bucket room for
    // four concurrent hedges, so the stranded set clears in seconds.
    const FILES: usize = 40;
    const FILE_SIZE: usize = 256 * 1024;
    // 5 KB/s, and the rate is low for ONE structural reason: at 25 KB/s the
    // peer finished a 1 MiB child in ~42 s, and a fetch running 108 s under the
    // suite's parallelism gave it time to — at which point it had MEASURED ITS
    // OWN GOODPUT, was thereafter "on time" by its own standard, was never
    // hedged again, and streamed for the rest of the run (2 829 577 B observed
    // against a 1.1 MB bound). The premise is a peer with a fast history that
    // is slow NOW, and it holds only while the peer completes nothing: at
    // 5 KB/s a 256 KiB child needs ~52 s, far longer than this fetch takes.
    const SLOW_RATE: u64 = 5_000;
    let rig = hedge_rig("hedge-cancel", FILES, FILE_SIZE, SLOW_RATE).await;

    let (telemetry, _) = recording_telemetry();
    let dest = tempdir().unwrap();
    let slow_before = sent_bytes(&rig.slow);
    let started = Instant::now();
    let report = tokio::time::timeout(
        // This test asserts a byte budget, not a clock, so a loaded runner must
        // not fail it — and at 120 s one did, twice, timing out at 127 s and
        // 128 s while the machine carried an unrelated load (the same tree
        // finished in 75 s once it was quiet). 240 s is patience for that,
        // chosen so it still cannot mask the defect this test exists for: a
        // primary that is never cancelled has to deliver all thirteen children
        // it is handed at 5 KB/s, which is about eleven minutes and blows any
        // timeout in this range. The slow peer also contributes a real term of
        // its own — a child that misses its hedge grinds at 5 KB/s for ~52 s —
        // so the margin is deliberately several multiples of the observed run.
        Duration::from_secs(240),
        rig.puller.fetch_collection_multi_tuned_for_test(
            Role::Recv,
            rig.providers.clone(),
            &rig.root.to_string(),
            rig.announce.byte_size,
            dest.path(),
            noop_fetch_sink(),
            telemetry,
            SwarmFetchMode::Assigned,
            Duration::from_secs(60),
            true,
        ),
    )
    .await
    .expect("the fetch must not hang")
    .expect("the fetch must complete")
    .expect("the assigned loop always reports");
    let elapsed = started.elapsed();
    let slow_sent = sent_bytes(&rig.slow).saturating_sub(slow_before);

    assert_package_landed(&rig.pkg_dir, dest.path(), FILES);
    assert!(
        report.hedges >= 1,
        "the test needs a hedge to have fired: {report:?}"
    );
    // Ruling R3, and the reason this assertion lives in the HEDGING test rather
    // than only in `assigned_fetch_report_matches_provider_send_counters`: this
    // is the run where a hedge's back half can be the last of its child. When
    // it is, the round that would otherwise do the crediting returns through
    // the top-of-loop `is_complete` early return and records nothing, so before
    // R3 such a child was counted for NOBODY and this sum came up short. The
    // collection is FILES payload entries plus `manifest.ndjson`.
    assert_eq!(
        report.total_children() as usize,
        FILES + 1,
        "every child must be credited exactly once, hedge-won ones included: \
         {report:?}"
    );

    // THE cancel oracle: the loser's own socket counter, never our telemetry.
    //
    // The bound is FOUR children, not the brief's one, and the difference is a
    // fact about the loop rather than slack: every time a hedge frees the slow
    // peer's assignment slot it becomes the least-loaded provider again and is
    // handed a FRESH child, so across forty children it is assigned roughly a
    // third of them and its cumulative egress scales with assignments, not with
    // one transfer. The brief's figure assumed a package small enough for a
    // peer to hold a fixed share. What the bound still says is the thing that
    // matters: cancelled after a few seconds each time, the peer never moved
    // more than four children's worth despite being handed thirteen — where an
    // uncancelled peer must deliver all thirteen at 5 KB/s, which is 11 minutes
    // and blows the harness timeout long before this line is reached.
    const CANCEL_BOUND: u64 = 4 * (FILE_SIZE as u64) + SERVED_PAYLOAD_FLOOR;
    assert!(
        slow_sent < CANCEL_BOUND,
        "a cancelled primary must stop sending — the slow provider put \
         {slow_sent} B on the wire in {elapsed:?}, over the {CANCEL_BOUND} B a \
         peer cancelled out of every assignment it was given can account for"
    );
    // A note for anyone tempted to replace that with a ratio against
    // `SLOW_RATE x elapsed`: I tried, and it is wrong. `sent_bytes` is the
    // ENDPOINT's total egress — QUIC handshake, ACKs, retransmits and this
    // peer's share of phase 1 — while `UploadPacer` caps only blob payload
    // writes. Measured: 236 349 B across 23.1 s from a peer capped at
    // 5 000 B/s, i.e. 10.2 KB/s on the socket against a 5 KB/s payload cap.
    // Any assertion of the form "less than rate x time" is therefore false on a
    // perfectly well-behaved run. The absolute bound above is the honest one,
    // and it is meaningful because the rate is now low enough that the peer
    // cannot complete a child and go un-hedged (see the rig comment).

    // And hedging did not double the package.
    let total = rig.announce.byte_size;
    assert!(
        report.hedge_bytes <= (total as f64 * 0.6) as u64,
        "hedge deliveries must stay a minority of the package: {} B of {total} B",
        report.hedge_bytes
    );

    rig.shutdown().await;
}

/// The bucket keeps a storm bounded: a peer that strands four or five children
/// at once does NOT produce one hedge per stranded child.
///
/// **On the brief's `hedges <= 1`: measured, and it is not a property of this
/// design.** Stranding all twelve children at once produces twelve hedges, and
/// that is correct — a hedge that WINS is refunded in full, because the primary
/// it cancelled was still below the split and duplicated nothing, so it costs
/// the bucket nothing and legitimately re-opens the gate for the next child.
/// The budget does not meter how OFTEN a useful hedge is taken; it meters the
/// extra BYTES, which is what gRPC's 5 % rule is about. So this test asserts
/// the byte bound, and asserting it end to end is also what pins the refund
/// rule: without refunds, twelve hedges of half a megabyte each would be six
/// megabytes of duplication against a 630 KB cap. The admission arithmetic
/// itself — `tokens > cap / 2 && tokens >= charge`, the cap, the refund — is
/// pinned exactly and deterministically by `assign.rs`'s own unit tests.
///
/// **A regime note worth keeping.** "The budget refuses every hedge AND the run
/// still finishes quickly" is not reachable: the three rules interlock. A peer
/// slow enough for the budget's charge to stay above the cap is a peer whose
/// missing range never shrinks, which means it is barely moving, which means
/// the store never learns the blob's size — and then the hedge is not refused
/// by the budget, it is never evaluated, and the STALL CEILING is what rescues
/// the child. That is the division of labour, not a gap.
#[tokio::test]
async fn hedge_budget_stops_a_storm() {
    // Sized like `hedge_fires_before_the_stall_ceiling_on_a_slow_provider` —
    // see its comment for why forty small children rather than twelve large
    // ones.
    const FILES: usize = 40;
    const FILE_SIZE: usize = 256 * 1024;
    let rig = hedge_rig("hedge-budget", FILES, FILE_SIZE, 400_000).await;
    rig.fast_a.set_upload_limit(400_000);
    rig.fast_b.set_upload_limit(400_000);

    let (telemetry, _) = recording_telemetry();
    let dest = tempdir().unwrap();
    // The storm: every child the slow peer holds is stranded at the same
    // instant, so every one of them reaches its hedge deadline together. The
    // collapse rides a timer task so the fetch can be awaited inline — see the
    // sibling test for why that matters on a timeout.
    let collapse = {
        let slow = Arc::clone(&rig.slow);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            slow.set_upload_limit(1_000);
        })
    };
    let report = tokio::time::timeout(
        // 120 s, matching the identically shaped
        // `hedge_fires_before_the_stall_ceiling_on_a_slow_provider`. At 60 s
        // this test's patience EQUALLED the stall ceiling it configures, so a
        // run in which the loop legitimately spent the ceiling on a provider
        // had nothing left for the rest of the fetch: observed hanging once in
        // six runs on a loaded machine. The claim here is a byte budget, not a
        // duration.
        Duration::from_secs(120),
        rig.puller.fetch_collection_multi_tuned_for_test(
            Role::Recv,
            rig.providers.clone(),
            &rig.root.to_string(),
            rig.announce.byte_size,
            dest.path(),
            noop_fetch_sink(),
            telemetry,
            SwarmFetchMode::Assigned,
            Duration::from_secs(60),
            true,
        ),
    )
    .await
    .expect("the fetch must not hang")
    .expect("the fetch must complete")
    .expect("the assigned loop always reports");
    collapse.await.ok();

    let report_cap_basis = rig.announce.byte_size;
    assert_package_landed(&rig.pkg_dir, dest.path(), FILES);
    assert!(
        report.hedges >= 1,
        "the test is only meaningful if hedging engaged at all: {report:?}"
    );
    // THE guarantee. `hedge_bytes` is what hedging DUPLICATED — every charge
    // the bucket did not get back — and the budget's whole job is to keep that
    // near `HEDGE_BUDGET_RATIO` of the package. The cumulative figure may reach
    // roughly twice the cap over a long run, because completed children refill
    // the bucket as they go; anything beyond that means charges are not being
    // refunded, i.e. hedges are duplicating whole ranges.
    //
    // MEASURED on this package (reviewer item 11, RE-measured after the
    // `armed_at` fix): three runs gave 22/20/22 hedges duplicating
    // 65 536 / 65 536 / 114 688 B against a cap of 524 288 B — 0.12-0.22 x the
    // cap, and 0.6-1.1 % of the ~10.5 MB payload, comfortably inside what a 5 %
    // budget promises.
    //
    // The earlier figure recorded here, 425 984 B over 11 hedges, was not a
    // measurement of this mechanism at all: duplication was computed against
    // `split_rel` alone, an offset into the missing set as it stood at ARM
    // time, while the primary's progress counts from the request it was given
    // at ROUND START. Everything the primary moved in between was charged as
    // duplicate. That inflated the reported cost ~4 x AND starved the bucket of
    // refunds, which is why the hedge count doubled once it was fixed: the
    // budget had been refusing hedges it could always have afforded.
    //
    // It is still not asserted exactly, because it moves with how much of each
    // stranded child the collapsed peer had already fetched when its hedge
    // fired — a timing fact, not a contract — and it rides the
    // `"assignment loop finished"` debug as `hedge_bytes` on every run.
    let cap = (report_cap_basis as f64 * 0.05) as u64;
    // Ruling R3 again, in the run that arms the most hedges (20-22 of them
    // across 41 children), so a hedge finishing the last of its own child is
    // near-certain here rather than incidental.
    assert_eq!(
        report.total_children() as usize,
        FILES + 1,
        "every child must be credited exactly once, hedge-won ones included: \
         {report:?}"
    );
    assert!(
        report.hedge_bytes <= 2 * cap,
        "hedging must stay inside its byte budget — it duplicated {} B against \
         a {cap} B cap (2x allowed for refills) over {} hedges: {report:?}",
        report.hedge_bytes,
        report.hedges
    );

    rig.shutdown().await;
}

/// The stall ceiling and the hedge budget are independent rules: whatever the
/// budget decides, the progress deadline fires on its own schedule and
/// reassigns the child.
///
/// The peer dribbles at 1 KB/s, so a 16 KiB chunk takes 16 s and the 1500 ms
/// deadline is unambiguous — and at that rate the store never learns the blob's
/// size in time either, so the hedge mostly cannot even be evaluated. What is
/// asserted is only the ceiling's own behaviour; see the body for why the hedge
/// COUNT is deliberately not part of the claim.
#[tokio::test]
async fn stall_ceiling_is_independent_of_the_hedge_budget() {
    const FILES: usize = 6;
    const FILE_SIZE: usize = 512 * 1024;
    // 1 KB/s, not 50: at 50 KB/s a 16 KiB chunk lands every 0.33 s, so a
    // 1500 ms progress deadline correctly never fires — that peer is slow, not
    // stuck. The ceiling's case is a peer that moves NOTHING for the window,
    // and at 1 KB/s one chunk takes 16 s.
    let rig = hedge_rig("hedge-ceiling", FILES, FILE_SIZE, 1_000).await;

    let (telemetry, _) = recording_telemetry();
    let dest = tempdir().unwrap();
    let report = tokio::time::timeout(
        Duration::from_secs(90),
        rig.puller.fetch_collection_multi_tuned_for_test(
            Role::Recv,
            rig.providers.clone(),
            &rig.root.to_string(),
            rig.announce.byte_size,
            dest.path(),
            noop_fetch_sink(),
            telemetry,
            SwarmFetchMode::Assigned,
            Duration::from_millis(1500),
            true,
        ),
    )
    .await
    .expect("the fetch must not hang")
    .expect("the fetch must complete")
    .expect("the assigned loop always reports");

    assert_package_landed(&rig.pkg_dir, dest.path(), FILES);
    // Hedging is ON throughout. The test deliberately does NOT assert
    // `hedges == 0`: a hedge's charge is half of what is still MISSING, and the
    // missing range shrinks every time the ceiling reassigns a child, so late
    // in the run a hedge does become affordable even on a package this small.
    // That is correct behaviour and it is beside the point here — this test's
    // claim is that the progress deadline fires on its own schedule, whatever
    // the budget is doing.
    let slow = report
        .per_provider
        .get(&endpoint_id(rig.slow_id))
        .cloned()
        .expect("the slow provider appears in the report");
    assert!(
        slow.failures >= 1,
        "the 1500 ms progress deadline must fail the trickling provider out \
         while hedging is enabled: {slow:?}"
    );
    assert!(
        report.stalls >= 1,
        "and that failure must be attributed to the deadline: {report:?}"
    );

    rig.shutdown().await;
}

// ---------------------------------------------------------------------------
// A2b Task 8 Step 1 — the HARD GATE for hedging.
//
// A hedge is a second transfer of part of a blob another transfer is already
// fetching. Everything above it is pointless unless the store tolerates two
// concurrent `import_bao` writers on ONE hash. iroh-blobs routes both through
// the same per-hash entity (`HashContext`), writes leaves at their own offsets
// and ORs the bitfield — so this SHOULD hold, but "should" is not a gate.
// ---------------------------------------------------------------------------

/// The collection entry named `name`, as the puller learns it from phase 1.
async fn child_hash_of(store: &Store, root: Hash, name: &str) -> Hash {
    let collection = Collection::load(root, store)
        .await
        .expect("the puller must be able to load the collection after phase 1");
    let found = collection.iter().find(|(n, _)| n == name).map(|(_, h)| *h);
    found.unwrap_or_else(|| panic!("collection has no entry {name}"))
}

/// Run `front` against provider A and `back` against provider B at the same
/// time, on the SAME blob, and return once both are done.
async fn two_range_gets(
    puller: &Arc<SharedIrohNode>,
    a: NodeId,
    b: NodeId,
    front: GetRequest,
    back: GetRequest,
) {
    let pool = ConnectionPool::new(
        puller.endpoint_for_test(),
        iroh_blobs::ALPN,
        PoolOptions::default(),
    );
    let conn_a = pool
        .get_or_connect(EndpointId::from_bytes(&a).unwrap())
        .await
        .expect("dial provider A");
    let conn_b = pool
        .get_or_connect(EndpointId::from_bytes(&b).unwrap())
        .await
        .expect("dial provider B");
    let remote = puller.store().remote().clone();
    let ga = remote.execute_get((*conn_a).clone(), front);
    let gb = remote.execute_get((*conn_b).clone(), back);
    let (ra, rb) = tokio::join!(ga, gb);
    ra.expect("the front-half get must succeed");
    rb.expect("the back-half get must succeed");
}

/// Two DISJOINT range gets on one blob, from two providers at once, leave the
/// blob complete and byte-correct. This is the gate: without it, hedging has to
/// become deadline-only (cancel and reassign the whole missing range).
#[tokio::test]
async fn two_disjoint_range_gets_on_one_blob_verify() {
    let da = tempdir().unwrap();
    let db = tempdir().unwrap();
    let dc = tempdir().unwrap();
    let src = tempdir().unwrap();

    let a = bind_disabled(da.path()).await;
    let b = bind_disabled(db.path()).await;
    let c = bind_disabled(dc.path()).await;

    let a_out = a.handle(Role::Out);
    let b_out = b.handle(Role::Out);
    let c_recv = c.handle(Role::Recv);
    let a_info = a_out.start().await.unwrap();
    let b_info = b_out.start().await.unwrap();
    let c_info = c_recv.start().await.unwrap();
    c.add_peer_ticket(&a_info.pairing_ticket).unwrap();
    c.add_peer_ticket(&b_info.pairing_ticket).unwrap();
    a.add_peer_ticket(&c_info.pairing_ticket).unwrap();
    b.add_peer_ticket(&c_info.pairing_ticket).unwrap();

    // ONE 4 MiB payload, served by both.
    const SIZE: usize = 4 * 1024 * 1024;
    let (pkg_dir, announce) = build_many_file_package(&src.path().join("src"), "split", 1, SIZE);
    a_out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    b_out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    let root = a
        .resolve_served_hash_for_test(Role::Out, &announce.package_id)
        .expect("provider A recorded a served collection hash");

    // Phase 1 only: the puller learns the entry names and child hashes, and
    // holds NONE of the payload.
    let mut real = announce.clone();
    real.root_hash = root.to_string();
    let manifest_dir = tempdir().unwrap();
    c_recv
        .fetch_manifest(a_info.node_id, &real, manifest_dir.path())
        .await
        .expect("phase 1 (root hash-seq + collection meta) must land");
    let hash = child_hash_of(c.store(), root, "frame_00.fits").await;

    // Split at the chunk-count midpoint. A BLAKE3 chunk is 1024 bytes, so a
    // 4 MiB blob is 4096 chunks and the midpoint is chunk 2048 — also a whole
    // number of 16 KiB verification groups, so neither half straddles one.
    let mid = (SIZE as u64 / 1024) / 2;
    let front = GetRequest::blob_ranges(hash, ChunkRanges::chunks(..mid));
    let back = GetRequest::blob_ranges(hash, ChunkRanges::chunks(mid..));
    two_range_gets(&c, a_info.node_id, b_info.node_id, front, back).await;

    // The store must now consider the blob whole...
    let remote = c.store().remote().clone();
    let local = remote
        .local_for_request(GetRequest::blob_ranges(hash, ChunkRanges::all()))
        .await
        .expect("local_for_request");
    assert!(
        local.is_complete(),
        "two disjoint writers must leave the blob COMPLETE — it is not, so the \
         hedge cannot split a range and must cancel-and-reassign instead"
    );

    // ...and the bytes must be the source's, not a seam of two half-writes.
    let landed = dc.path().join("rejoined.fits");
    c.store()
        .blobs()
        .export(hash, &landed)
        .await
        .expect("a blob the store calls complete must export");
    assert_eq!(
        xxh3_of(&pkg_dir.join("frame_00.fits")),
        xxh3_of(&landed),
        "the rejoined halves must hash-match the source byte for byte"
    );

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

/// The case the hedge actually creates: the halves OVERLAP, because the primary
/// keeps its whole-blob request and may cross the midpoint before the hedge
/// finishes. Same store, same entry, same verified bytes — this pins that an
/// overlapping second writer is idempotent rather than corrupting.
#[tokio::test]
async fn overlapping_range_gets_on_one_blob_verify() {
    let da = tempdir().unwrap();
    let db = tempdir().unwrap();
    let dc = tempdir().unwrap();
    let src = tempdir().unwrap();

    let a = bind_disabled(da.path()).await;
    let b = bind_disabled(db.path()).await;
    let c = bind_disabled(dc.path()).await;

    let a_out = a.handle(Role::Out);
    let b_out = b.handle(Role::Out);
    let c_recv = c.handle(Role::Recv);
    let a_info = a_out.start().await.unwrap();
    let b_info = b_out.start().await.unwrap();
    let c_info = c_recv.start().await.unwrap();
    c.add_peer_ticket(&a_info.pairing_ticket).unwrap();
    c.add_peer_ticket(&b_info.pairing_ticket).unwrap();
    a.add_peer_ticket(&c_info.pairing_ticket).unwrap();
    b.add_peer_ticket(&c_info.pairing_ticket).unwrap();

    const SIZE: usize = 4 * 1024 * 1024;
    let (pkg_dir, announce) = build_many_file_package(&src.path().join("src"), "overlap", 1, SIZE);
    a_out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    b_out.serve(&announce, &pkg_dir, None, None).await.unwrap();
    let root = a
        .resolve_served_hash_for_test(Role::Out, &announce.package_id)
        .expect("provider A recorded a served collection hash");

    let mut real = announce.clone();
    real.root_hash = root.to_string();
    let manifest_dir = tempdir().unwrap();
    c_recv
        .fetch_manifest(a_info.node_id, &real, manifest_dir.path())
        .await
        .expect("phase 1 must land");
    let hash = child_hash_of(c.store(), root, "frame_00.fits").await;

    // A asked for EVERYTHING (the primary's request), B for the back half (the
    // hedge's) — so every chunk from the midpoint on is written twice.
    let mid = (SIZE as u64 / 1024) / 2;
    let whole = GetRequest::blob_ranges(hash, ChunkRanges::all());
    let back = GetRequest::blob_ranges(hash, ChunkRanges::chunks(mid..));
    two_range_gets(&c, a_info.node_id, b_info.node_id, whole, back).await;

    let remote = c.store().remote().clone();
    let local = remote
        .local_for_request(GetRequest::blob_ranges(hash, ChunkRanges::all()))
        .await
        .expect("local_for_request");
    assert!(
        local.is_complete(),
        "an overlapping second writer must not leave the blob incomplete"
    );
    let landed = dc.path().join("overlapped.fits");
    c.store()
        .blobs()
        .export(hash, &landed)
        .await
        .expect("export");
    assert_eq!(
        xxh3_of(&pkg_dir.join("frame_00.fits")),
        xxh3_of(&landed),
        "two writers of the SAME verified chunks must be idempotent, not a seam"
    );

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

/// Transfer-prepare spec §4.1: the two-dir bind keeps the device IDENTITY under
/// `identity_dir` (the key and its advisory-lock sidecar never move, so a
/// relocated working folder can never mint a second identity) while every byte
/// of data — the blob store here — follows `working_dir`.
#[tokio::test]
async fn bind_with_keeps_identity_in_identity_dir_and_blobs_in_working_dir() {
    let tmp = tempdir().unwrap();
    let identity = tmp.path().join("identity");
    let working = tmp.path().join("working");
    let node = SharedIrohNode::bind_with(
        &identity,
        &working,
        RelayMode::Disabled,
        NodeOptions::default(),
    )
    .await
    .unwrap();
    assert!(
        identity.join("device_key").is_file(),
        "key under identity dir"
    );
    assert!(
        !working.join("device_key").exists(),
        "no key under working dir"
    );
    assert!(
        working.join("blobs").join("blobs.db").is_file(),
        "store under working dir"
    );
    assert_eq!(node.working_dir(), working.as_path());
    assert_eq!(
        node.serve_import_mode(),
        iroh_blobs::api::blobs::ImportMode::Copy
    );
    node.shutdown().await;
}

/// Build a package under `root/pkg` from synthetic payloads written to
/// `root/src`. Each payload is above the fs store's inline threshold (16 KiB),
/// so the store must genuinely either copy the bytes in or reference them where
/// they lie — which is what the import-mode tests below measure.
fn write_test_package(root: &std::path::Path, files: &[(&str, usize)]) -> std::path::PathBuf {
    write_test_package_announced(root, files).0
}

/// [`write_test_package`] plus the [`PackageAnnounce`] `write_package` minted for
/// it (a fresh uuid per call, so two packages never collide on a serve tag).
fn write_test_package_announced(
    root: &std::path::Path,
    files: &[(&str, usize)],
) -> (std::path::PathBuf, PackageAnnounce) {
    use crate::package::{write_package, ManifestRecord, PayloadKind, MANIFEST_VERSION};
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut records = Vec::new();
    for (name, size) in files {
        let p = src.join(name);
        let bytes: Vec<u8> = (0..*size).map(|i| (i % 253) as u8).collect();
        std::fs::write(&p, &bytes).unwrap();
        records.push((
            p.clone(),
            ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: format!("u-{name}"),
                origin_catalog_uuid: format!("u-{name}"),
                origin_device: "dev".into(),
                payload_kind: PayloadKind::RawFrame,
                rel_path: name.to_string(),
                byte_size: *size as u64,
                xxh3: crate::package::xxh3_full_file(&p).unwrap(),
                frame_meta: serde_json::json!({}),
                analysis: None,
                app_version: "test".into(),
                project: None,
            },
        ));
    }
    let pkg = root.join("pkg");
    let announce = write_package(&pkg, records).unwrap();
    (pkg, announce)
}

/// Does the blob store hold a file of exactly `size` bytes — i.e. a second copy
/// of a payload? `TryReference` stores only the outboard (64 B per 16 KiB), so a
/// referenced payload never shows up here.
fn store_holds_payload_copy(blob_dir: &std::path::Path, size: u64) -> bool {
    walkdir::WalkDir::new(blob_dir)
        .into_iter()
        .flatten()
        .any(|e| e.file_type().is_file() && e.metadata().map(|m| m.len() == size).unwrap_or(false))
}

/// Transfer-prepare spec §4.1: an app-host serve imports the prepared package
/// dir by REFERENCE — the store gains metadata (collection, hash-seq, outboards),
/// not a second copy of every frame — and the mode never moves the hash, so the
/// announced `root_hash` is identical either way. The want-subset import (the
/// dedup-negotiated send) honors the same mode; it used to call `add_path`,
/// i.e. an unconditional Copy on every subset send.
#[tokio::test]
async fn try_reference_import_yields_same_hash_and_no_store_copy() {
    use crate::sharing::iroh::blobs::{
        import_package_collection_with_mode, import_subset_collection,
    };
    use iroh_blobs::api::blobs::ImportMode;
    let tmp = tempfile::tempdir().unwrap();
    let pkg = write_test_package(tmp.path(), &[("a.fits", 300_000), ("b.fits", 300_000)]);

    let copy_store = iroh_blobs::store::fs::FsStore::load(tmp.path().join("copy"))
        .await
        .unwrap();
    let ref_store = iroh_blobs::store::fs::FsStore::load(tmp.path().join("reference"))
        .await
        .unwrap();
    let (h_copy, _) =
        import_package_collection_with_mode(&copy_store, &pkg, "t", ImportMode::Copy, None)
            .await
            .unwrap();
    let (h_ref, _) =
        import_package_collection_with_mode(&ref_store, &pkg, "t", ImportMode::TryReference, None)
            .await
            .unwrap();
    assert_eq!(h_copy, h_ref, "mode never changes the collection hash");
    assert!(store_holds_payload_copy(&tmp.path().join("copy"), 300_000));
    assert!(
        !store_holds_payload_copy(&tmp.path().join("reference"), 300_000),
        "reference: no 300 000-byte file in the store"
    );

    // The want-subset import honors the mode too (it used to call add_path = Copy).
    let sub_store = iroh_blobs::store::fs::FsStore::load(tmp.path().join("subset"))
        .await
        .unwrap();
    let want: std::collections::HashSet<String> = ["a.fits".to_string()].into_iter().collect();
    import_subset_collection(&sub_store, &pkg, &want, "t", ImportMode::TryReference, None)
        .await
        .unwrap();
    assert!(!store_holds_payload_copy(
        &tmp.path().join("subset"),
        300_000
    ));
}

/// Import a file by reference and return its temp tag — the one-liner the
/// re-import test repeats.
async fn add_by_reference(
    store: &iroh_blobs::store::fs::FsStore,
    path: &std::path::Path,
) -> iroh_blobs::api::TempTag {
    use iroh_blobs::api::blobs::{AddPathOptions, ImportMode};
    store
        .blobs()
        .add_path_with_opts(AddPathOptions {
            path: path.to_path_buf(),
            format: iroh_blobs::BlobFormat::Raw,
            mode: ImportMode::TryReference,
        })
        .temp_tag()
        .await
        .unwrap()
}

/// What re-importing a KNOWN hash from a new path actually does in iroh-blobs
/// 0.103 — the lifecycle question `TryReference` hangs on (spec §4.2).
///
/// It does NOT re-point the entry. `finish_import_impl` builds
/// `DataLocation::External(vec![new_path], size)`, but the meta actor
/// (`handle_update` → `EntryState::union` → `DataLocation::union`) UNIONS it
/// with what was there, then **sorts and dedups** the path list; every reader
/// (`export_path_impl`, and `BaoFileStorage::open` when the in-memory handle has
/// to be reloaded) takes `paths.first()`. So:
///
/// - re-importing the SAME path is idempotent (dedup) — the cancel/resend case;
/// - re-importing a live path that sorts BEFORE a vanished one heals the entry;
/// - a vanished path that sorts FIRST keeps losing, and the entry stays
///   unreadable from disk until GC drops it.
///
/// The last bullet is a real exposure for repeat sends of the same bytes from
/// two different `packages/<uuid>` dirs (and for the declined-divert rename).
/// It bites immediately — the failure is not deferred to a restart — which is
/// what makes the one-byte probe in `blobs::ensure_child_readable` able to catch
/// and repair it at import time.
#[tokio::test]
async fn reimport_of_a_known_hash_unions_external_paths_and_reads_the_first() {
    let tmp = tempfile::tempdir().unwrap();
    let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();

    // The live path sorts FIRST: the entry heals, the blob reads back.
    let healed = iroh_blobs::store::fs::FsStore::load(tmp.path().join("healed"))
        .await
        .unwrap();
    let stale = tmp.path().join("z_stale.bin");
    let live = tmp.path().join("a_live.bin");
    std::fs::write(&stale, &bytes).unwrap();
    let tag1 = add_by_reference(&healed, &stale).await;
    std::fs::remove_file(&stale).unwrap();
    std::fs::write(&live, &bytes).unwrap();
    let tag2 = add_by_reference(&healed, &live).await;
    assert_eq!(tag1.hash(), tag2.hash(), "path never moves the hash");
    let out = tmp.path().join("out.bin");
    healed.blobs().export(tag2.hash(), &out).await.unwrap();
    assert_eq!(
        std::fs::read(&out).unwrap(),
        bytes,
        "read from the live path once it sorts first"
    );

    // Same bytes, same sequence, only the names swapped so the VANISHED path
    // sorts first — the re-import does not displace it and the read fails.
    let stuck = iroh_blobs::store::fs::FsStore::load(tmp.path().join("stuck"))
        .await
        .unwrap();
    let stale2 = tmp.path().join("a_stale.bin");
    let live2 = tmp.path().join("z_live.bin");
    std::fs::write(&stale2, &bytes).unwrap();
    let tag3 = add_by_reference(&stuck, &stale2).await;
    std::fs::remove_file(&stale2).unwrap();
    std::fs::write(&live2, &bytes).unwrap();
    let tag4 = add_by_reference(&stuck, &live2).await;
    assert_eq!(tag3.hash(), tag4.hash());
    let out2 = tmp.path().join("out2.bin");
    assert!(
        stuck.blobs().export(tag4.hash(), &out2).await.is_err(),
        "union, not re-point: the stale first path still wins the read"
    );

    // Re-importing the SAME path is idempotent — the resend-the-same-dir case.
    let again = add_by_reference(&healed, &live).await;
    assert_eq!(again.hash(), tag2.hash());
    let out3 = tmp.path().join("out3.bin");
    healed.blobs().export(tag2.hash(), &out3).await.unwrap();
    assert_eq!(std::fs::read(&out3).unwrap(), bytes);
}

/// The mitigation for the union semantics pinned above: after a `TryReference`
/// import the app probes one byte per child, and a child that reads a DEAD path
/// is re-imported with `Copy` — `Owned` wins the union, so the entry is repaired
/// permanently instead of staying unreadable until GC.
///
/// Both directions are asserted, because the probe (not an unconditional copy)
/// is what gates the repair: with the stale path sorting FIRST the store ends up
/// holding an owned copy and reads survive deleting every source file; with the
/// stale path sorting LAST nothing is copied and reads work as they always did.
#[tokio::test]
async fn dead_first_external_path_is_repaired_by_a_copy_reimport() {
    use crate::sharing::iroh::blobs::import_package_collection_with_mode;
    use iroh_blobs::api::blobs::ImportMode;
    use iroh_blobs::store::fs::FsStore;

    const SIZE: usize = 300_000;
    // Byte-for-byte what `write_test_package` writes, so the stale file and the
    // package payload are the same blob.
    let payload: Vec<u8> = (0..SIZE).map(|i| (i % 253) as u8).collect();

    async fn readable(store: &FsStore, hash: Hash) -> bool {
        store
            .blobs()
            .export_ranges(hash, 0..1u64)
            .concatenate()
            .await
            .is_ok()
    }

    // --- stale path sorts FIRST ("aaa_stale.bin" < "pkg/a.fits") → repair runs.
    let tmp = tempdir().unwrap();
    let pkg = write_test_package(tmp.path(), &[("a.fits", SIZE)]);
    let store_dir = tmp.path().join("store");
    let store = FsStore::load(&store_dir).await.unwrap();
    let stale = tmp.path().join("aaa_stale.bin");
    std::fs::write(&stale, &payload).unwrap();
    let hash = add_by_reference(&store, &stale).await.hash();
    std::fs::remove_file(&stale).unwrap();
    assert!(
        !readable(&store, hash).await,
        "precondition: the entry reads the now-deleted first path"
    );

    import_package_collection_with_mode(&store, &pkg, "t", ImportMode::TryReference, None)
        .await
        .unwrap();
    assert!(
        store_holds_payload_copy(&store_dir, SIZE as u64),
        "a dead first path is repaired by copying the payload into the store"
    );
    std::fs::remove_file(pkg.join("a.fits")).unwrap();
    assert!(
        readable(&store, hash).await,
        "owned after the repair: readable with no source file left on disk"
    );

    // --- stale path sorts LAST ("pkg/a.fits" < "zzz_stale.bin") → no repair.
    let tmp2 = tempdir().unwrap();
    let pkg2 = write_test_package(tmp2.path(), &[("a.fits", SIZE)]);
    let store_dir2 = tmp2.path().join("store");
    let store2 = FsStore::load(&store_dir2).await.unwrap();
    let stale2 = tmp2.path().join("zzz_stale.bin");
    std::fs::write(&stale2, &payload).unwrap();
    let hash2 = add_by_reference(&store2, &stale2).await.hash();
    std::fs::remove_file(&stale2).unwrap();

    import_package_collection_with_mode(&store2, &pkg2, "t", ImportMode::TryReference, None)
        .await
        .unwrap();
    assert!(
        !store_holds_payload_copy(&store_dir2, SIZE as u64),
        "a healthy probe takes no copy — the probe is the gate, not the mode"
    );
    assert!(
        readable(&store2, hash2).await,
        "still served from the live package path"
    );
}

/// Transfer-prepare spec §4.4: the serve import reports byte progress so a
/// multi-GB package's outboard-hashing pass is visible as the `indexing` stage
/// instead of a frozen row. Ticks are throttled, so the only figure a consumer
/// can rely on is the terminal one — which must pin `done == total`, and `total`
/// must be everything the import actually read off disk (payloads AND the
/// package's own `manifest.ndjson`, which `collect_files` walks like any other
/// file).
#[tokio::test]
async fn import_reports_byte_progress_reaching_the_total() {
    use crate::sharing::iroh::blobs::import_package_collection_with_mode;
    use iroh_blobs::api::blobs::ImportMode;
    let tmp = tempfile::tempdir().unwrap();
    let pkg = write_test_package(tmp.path(), &[("a.fits", 2_000_000), ("b.fits", 2_000_000)]);
    let store = iroh_blobs::store::fs::FsStore::load(tmp.path().join("s"))
        .await
        .unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(u64, u64)>::new()));
    let sink = {
        let seen = seen.clone();
        std::sync::Arc::new(move |done: u64, total: u64| seen.lock().unwrap().push((done, total)))
    };
    import_package_collection_with_mode(&store, &pkg, "t", ImportMode::TryReference, Some(sink))
        .await
        .unwrap();
    let ticks = seen.lock().unwrap().clone();
    let last = ticks.last().copied().expect("at least the terminal tick");
    assert_eq!(
        last.1,
        4_000_000
            + std::fs::metadata(pkg.join("manifest.ndjson"))
                .unwrap()
                .len()
    );
    assert_eq!(last.0, last.1, "terminal tick pins done == total");
}

/// A node that imports serves BY REFERENCE, the way the app's
/// `ensure_iroh_node` binds it (`SharedIrohNode::bind` — Perseus and the other
/// tests here — stays `Copy`).
async fn bind_try_reference(dir: &Path) -> Arc<SharedIrohNode> {
    SharedIrohNode::bind_with(
        dir,
        dir,
        RelayMode::Disabled,
        NodeOptions {
            serve_import_mode: iroh_blobs::api::blobs::ImportMode::TryReference,
        },
    )
    .await
    .expect("bind relay-disabled TryReference node")
}

/// The child hash a served collection carries under `rel_path`.
async fn served_child_hash(node: &SharedIrohNode, package_id: &PackageId, rel_path: &str) -> Hash {
    let root = node
        .resolve_served_hash_for_test(Role::Out, package_id)
        .expect("package is served");
    let coll = Collection::load(root, node.store())
        .await
        .expect("load served collection");
    let found = coll
        .iter()
        .find(|(name, _)| name == rel_path)
        .map(|(_, h)| *h);
    found.unwrap_or_else(|| panic!("{rel_path} not in the served collection"))
}

/// Can the store still read this blob's first byte? (`has`/`status` cannot tell
/// — they answer from the metadata row.)
async fn blob_readable(node: &SharedIrohNode, hash: Hash) -> bool {
    node.store()
        .blobs()
        .export_ranges(hash, 0..1u64)
        .concatenate()
        .await
        .is_ok()
}

/// Deleting a confirmed package's payloads must not strip another live package
/// of its bytes.
///
/// Two packages carrying the same frame share ONE store entry, whose external
/// path list is `[A's copy, B's copy]` sorted — so the read follows A's payload
/// dir, which the confirm then deletes. B's tag keeps the entry alive past GC, so
/// nothing would heal it. `protect_shared_before_cleanup` — which the engine
/// calls immediately before that deletion — copies exactly the shared children
/// into the store (`Owned`), and nothing else.
#[tokio::test]
async fn protect_before_cleanup_copies_children_shared_with_another_live_package() {
    const SHARED: usize = 300_000;
    // A different size ⇒ different bytes ⇒ a different blob, so the two
    // `store_holds_payload_copy` probes below cannot answer for each other.
    const ONLY_A: usize = 290_000;

    let tmp = tempdir().unwrap();
    let node_dir = tmp.path().join("node");
    let node = bind_try_reference(&node_dir).await;
    let out = node.handle(Role::Out);

    // "aaa" < "zzz", so A's payload path is the one the union reads.
    let (pkg_a, ann_a) = write_test_package_announced(
        &tmp.path().join("aaa"),
        &[("X.fits", SHARED), ("only_a.fits", ONLY_A)],
    );
    let (pkg_b, ann_b) =
        write_test_package_announced(&tmp.path().join("zzz"), &[("X.fits", SHARED)]);
    out.serve(&ann_a, &pkg_a, None, None).await.unwrap();
    out.serve(&ann_b, &pkg_b, None, None).await.unwrap();
    let shared_hash = served_child_hash(&node, &ann_b.package_id, "X.fits").await;

    // The engine's order: protect, then delete the payloads, then release.
    out.protect_shared_before_cleanup(&ann_a.package_id)
        .await
        .unwrap();
    std::fs::remove_dir_all(&pkg_a).unwrap();
    out.release(&ann_a.package_id).await.unwrap();

    let store_dir = node_dir.join("blobs");
    assert!(
        store_holds_payload_copy(&store_dir, SHARED as u64),
        "the child B also serves was copied into the store before the release"
    );
    assert!(
        !store_holds_payload_copy(&store_dir, ONLY_A as u64),
        "a child only the released package served is left referenced — no blanket copy"
    );
    assert!(
        blob_readable(&node, shared_hash).await,
        "B still serves the shared blob after A's payload dir is gone"
    );

    node.shutdown().await;
}

/// The protection's fallback source: if our own payload dir is already gone (a
/// crash between cleanup and a resumed release, an out-of-band purge), the bytes
/// come from the SHARING package's payload — same blob, still on disk because
/// that package's tag is live.
#[tokio::test]
async fn protect_copies_shared_children_from_the_sharer_when_our_dir_is_gone() {
    const SHARED: usize = 300_000;

    let tmp = tempdir().unwrap();
    let node_dir = tmp.path().join("node");
    let node = bind_try_reference(&node_dir).await;
    let out = node.handle(Role::Out);

    let (pkg_a, ann_a) =
        write_test_package_announced(&tmp.path().join("aaa"), &[("X.fits", SHARED)]);
    let (pkg_b, ann_b) =
        write_test_package_announced(&tmp.path().join("zzz"), &[("X.fits", SHARED)]);
    out.serve(&ann_a, &pkg_a, None, None).await.unwrap();
    out.serve(&ann_b, &pkg_b, None, None).await.unwrap();
    let shared_hash = served_child_hash(&node, &ann_b.package_id, "X.fits").await;

    // Our dir is already gone when the protection runs.
    std::fs::remove_dir_all(&pkg_a).unwrap();
    out.protect_shared_before_cleanup(&ann_a.package_id)
        .await
        .unwrap();

    assert!(
        store_holds_payload_copy(&node_dir.join("blobs"), SHARED as u64),
        "copied from the sharing package's payload"
    );
    assert!(blob_readable(&node, shared_hash).await);

    node.shutdown().await;
}

/// The re-serve short-circuit reuses an already-imported collection and so skips
/// the import's `ensure_child_readable` repair. It therefore probes first: a
/// collection whose referenced files went away is re-imported rather than
/// re-announced dead.
#[tokio::test]
async fn reserve_after_a_dead_path_repairs_it() {
    const SHARED: usize = 300_000;

    let tmp = tempdir().unwrap();
    let node_dir = tmp.path().join("node");
    let node = bind_try_reference(&node_dir).await;
    let out = node.handle(Role::Out);

    let (pkg_a, ann_a) =
        write_test_package_announced(&tmp.path().join("aaa"), &[("X.fits", SHARED)]);
    let (pkg_b, ann_b) =
        write_test_package_announced(&tmp.path().join("zzz"), &[("X.fits", SHARED)]);
    out.serve(&ann_a, &pkg_a, None, None).await.unwrap();
    out.serve(&ann_b, &pkg_b, None, None).await.unwrap();
    let shared_hash = served_child_hash(&node, &ann_b.package_id, "X.fits").await;

    // The window the release protection does not cover: A's dir vanishes with no
    // release at all (a crash between cleanup and release, a manual purge).
    std::fs::remove_dir_all(&pkg_a).unwrap();
    assert!(
        !blob_readable(&node, shared_hash).await,
        "precondition: the shared entry now reads A's deleted path"
    );

    // A retry re-serves B with the same want — the short-circuit must notice.
    out.serve(&ann_b, &pkg_b, None, None).await.unwrap();
    assert!(
        blob_readable(&node, shared_hash).await,
        "the re-serve re-imported and repaired the dead path"
    );

    node.shutdown().await;
}

/// A want-subset serve's `manifest.ndjson` is SYNTHESIZED in memory
/// (`import_subset_collection` filters the records and `add_bytes` them), so the
/// file of that name under `src_dir` is a different document — the full manifest.
/// Two subset serves of the same frames with the same want set produce the
/// identical filtered manifest (no record carries a package-unique field), so it
/// looks "shared"; above the inline threshold the protection would then copy the
/// wrong file, mismatch the hash and fail the whole pass — permanently skipping
/// the cleanup of a healthy confirmed package. An `add_bytes` blob is never
/// external, so it is skipped instead.
#[tokio::test]
async fn protect_skips_the_synthesized_subset_manifest() {
    const BIG: usize = 300_000;
    const RECORDS: usize = 200;

    let tmp = tempdir().unwrap();
    let node_dir = tmp.path().join("node");
    let node = bind_try_reference(&node_dir).await;
    let out = node.handle(Role::Out);

    // Enough records that the filtered manifest is well above the 16 KiB inline
    // threshold, plus one real payload big enough to be external.
    let mut files: Vec<(String, usize)> = (0..RECORDS - 1)
        .map(|i| (format!("f{i:03}.fits"), 100 + i))
        .collect();
    files.push(("big.fits".to_string(), BIG));
    let files: Vec<(&str, usize)> = files.iter().map(|(n, s)| (n.as_str(), *s)).collect();

    let (pkg_a, ann_a) = write_test_package_announced(&tmp.path().join("aaa"), &files);
    let (pkg_b, ann_b) = write_test_package_announced(&tmp.path().join("zzz"), &files);
    assert!(
        std::fs::metadata(pkg_a.join("manifest.ndjson"))
            .unwrap()
            .len()
            > 16 * 1024,
        "the manifest must exceed the inline threshold or this test proves nothing"
    );

    // A STRICT subset, so the filtered manifest differs from the on-disk one.
    let want: std::collections::HashSet<String> = files
        .iter()
        .map(|(n, _)| n.to_string())
        .filter(|n| n != "f000.fits")
        .collect();
    out.serve(&ann_a, &pkg_a, Some(&want), None).await.unwrap();
    out.serve(&ann_b, &pkg_b, Some(&want), None).await.unwrap();
    let shared_hash = served_child_hash(&node, &ann_b.package_id, "big.fits").await;

    // Before the skip this returned Err on the manifest's hash mismatch.
    out.protect_shared_before_cleanup(&ann_a.package_id)
        .await
        .unwrap();
    std::fs::remove_dir_all(&pkg_a).unwrap();
    out.release(&ann_a.package_id).await.unwrap();

    assert!(
        store_holds_payload_copy(&node_dir.join("blobs"), BIG as u64),
        "the shared payload child is still protected — the manifest skip must not \
         short-circuit the rest of the pass"
    );
    assert!(
        blob_readable(&node, shared_hash).await,
        "B still serves the shared payload after A's dir is gone"
    );

    node.shutdown().await;
}

/// Transfer-prepare spec §5.1/§5.3: the receiver's export MOVES the store-owned
/// data file into staging (`ExportMode::TryReference`) instead of copying it, so
/// an inbound package never costs two copies of every frame on disk. The store
/// then references the staged file — and once that external path is gone (a
/// same-hash sibling package's staged file cleaned before this export ran, GC not
/// yet caught up), the export fails with a *vanished source*, which
/// [`export_source_vanished`] must recognize so the row parks `Waiting` instead
/// of being terminalized `Failed`.
#[tokio::test]
async fn export_try_reference_leaves_no_owned_copy_in_the_store() {
    use iroh_blobs::api::blobs::{ExportMode, ExportOptions};
    let tmp = tempfile::tempdir().unwrap();
    let store = iroh_blobs::store::fs::FsStore::load(tmp.path().join("s"))
        .await
        .unwrap();
    let bytes: Vec<u8> = (0..500_000u32).map(|i| (i % 249) as u8).collect();
    let tag = store
        .blobs()
        .add_bytes(bytes.clone())
        .temp_tag()
        .await
        .unwrap();
    assert!(
        store_holds_payload_copy(&tmp.path().join("s"), 500_000),
        "owned before export"
    );
    let target = tmp.path().join("staging").join("a.fits");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    store
        .blobs()
        .export_with_opts(ExportOptions {
            hash: tag.hash(),
            mode: ExportMode::TryReference,
            target: target.clone(),
        })
        .finish()
        .await
        .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
    assert!(
        !store_holds_payload_copy(&tmp.path().join("s"), 500_000),
        "moved out: the store no longer owns a copy"
    );
    // A second export of the same hash copies FROM the external path.
    let target2 = tmp.path().join("staging").join("b.fits");
    store
        .blobs()
        .export_with_opts(ExportOptions {
            hash: tag.hash(),
            mode: ExportMode::TryReference,
            target: target2.clone(),
        })
        .finish()
        .await
        .unwrap();
    assert_eq!(std::fs::read(&target2).unwrap(), bytes);
    // And once that external path is gone, the export fails with a vanished source.
    std::fs::remove_file(&target).unwrap();
    std::fs::remove_file(&target2).unwrap();
    let target3 = tmp.path().join("staging").join("c.fits");
    let err = store
        .blobs()
        .export_with_opts(ExportOptions {
            hash: tag.hash(),
            mode: ExportMode::TryReference,
            target: target3,
        })
        .finish()
        .await
        .unwrap_err();
    assert!(
        crate::sharing::iroh::blobs::export_source_vanished(&err),
        "{err:?}"
    );
}

/// Transfer-prepare spec §5.3: a vanished-source export must leave the receiver
/// able to HEAL. An unmarked (transfer-class) error parks the inbound row
/// `Waiting`, and a park never calls `release` — so if the fetch kept its own
/// collection tag, the dead child entry would stay pinned against GC and every
/// sender retry would re-run the same failing export (the downloader skips a blob
/// whose entry reads `Complete`, dead file or not). `on_export_source_vanished`
/// therefore drops that tag, leaving the entry untagged for the next GC pass.
///
/// The error handed to it here is a REAL one: a `TryReference` export of a blob
/// whose only external path has been deleted, exactly as the export loop
/// produces it. Driving the whole of `fetch_collection_to_dir` into this branch
/// is not possible — a re-fetch over such an entry trips an iroh-blobs 0.103
/// panic (`bitfield()` on `BaoFileStorage::Poisoned`, reached through our own
/// per-file `observe`) during phase 2, before the export loop runs.
#[tokio::test]
async fn vanished_export_drops_the_collection_tag() {
    use iroh_blobs::api::blobs::{ExportMode, ExportOptions};
    use iroh_blobs::HashAndFormat;

    let tmp = tempdir().unwrap();
    let store = iroh_blobs::store::fs::FsStore::load(tmp.path().join("s"))
        .await
        .unwrap();
    // Above the inline threshold, so the store keeps a real data file that
    // `TryReference` can move out and then reference.
    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let tt = store
        .blobs()
        .add_bytes(bytes.clone())
        .temp_tag()
        .await
        .unwrap();
    let target = tmp.path().join("staging").join("a.fits");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    store
        .blobs()
        .export_with_opts(ExportOptions {
            hash: tt.hash(),
            mode: ExportMode::TryReference,
            target: target.clone(),
        })
        .finish()
        .await
        .unwrap();

    // The tag the fetch sets on the collection just before the export loop.
    let tag = "pkg/vanished-probe";
    store
        .tags()
        .set(tag, HashAndFormat::hash_seq(tt.hash()))
        .await
        .unwrap();
    assert!(
        store.tags().get(tag.as_bytes()).await.unwrap().is_some(),
        "precondition: the fetch has pinned the collection"
    );

    // Now the staged file the store references is cleaned by a sibling package —
    // §5.3 — and the next export of that hash finds nothing to read.
    std::fs::remove_file(&target).unwrap();
    let err = store
        .blobs()
        .export_with_opts(ExportOptions {
            hash: tt.hash(),
            mode: ExportMode::TryReference,
            target: tmp.path().join("staging").join("b.fits"),
        })
        .finish()
        .await
        .unwrap_err();
    assert!(
        super::blobs::export_source_vanished(&err),
        "precondition: a real vanished-source export error, got {err:?}"
    );

    let out = super::blobs::on_export_source_vanished(
        &store,
        tag,
        tt.hash(),
        "a.fits",
        tt.hash(),
        &target,
        err,
    )
    .await;

    assert!(
        store.tags().get(tag.as_bytes()).await.unwrap().is_none(),
        "the collection tag is dropped, so GC can purge the dead entry and a \
         later retry re-downloads the blob"
    );
    assert!(
        !crate::sharing::types::is_local_fault(&out),
        "a vanished source is transfer-class: the row parks Waiting, it is never \
         terminalized Failed"
    );
    let msg = format!("{out:#}");
    assert!(msg.contains("source vanished"), "{msg}");
}

/// A retry re-runs the export loop over a staging dir nobody cleaned (a `Waiting`
/// park leaves `<root>/staging/<wire_id>` in place), so a child an earlier attempt
/// already exported is re-exported into the SAME target — and that target is now
/// the entry's own external path. Upstream would take its non-empty-external arm
/// and `reflink_or_copy(source_path, target)` with `source_path == target`:
/// `File::create(to)` truncates the inode to zero before the read, which both
/// fails the export with a NON-`NotFound` io error (⇒ `LocalFault` ⇒ the row is
/// terminalized `Failed`, when the truth is recoverable) and leaves the entry
/// pointing at a 0-byte file, wedging every sibling package sharing that hash.
///
/// `export_child` — the exact code the loop runs — removes a stale target first,
/// so the pathological self-copy cannot happen: the re-export degrades to the
/// vanished-source class, which self-heals (§5.3). Exporting to a DIFFERENT target
/// still works, so the guard costs the legitimate copy path nothing.
#[tokio::test]
async fn re_export_into_the_same_staging_target_does_not_truncate_it() {
    let tmp = tempdir().unwrap();
    let store = iroh_blobs::store::fs::FsStore::load(tmp.path().join("s"))
        .await
        .unwrap();
    // Above the inline threshold, so the store keeps a real data file that
    // `TryReference` moves out and then references.
    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let tt = store
        .blobs()
        .add_bytes(bytes.clone())
        .temp_tag()
        .await
        .unwrap();
    let staging = tmp.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let target = staging.join("a.fits");

    // First attempt: the payload moves out of the store into staging.
    super::blobs::export_child(&store, tt.hash(), &target)
        .await
        .expect("no local pre-step failure")
        .expect("the first export lands");
    assert_eq!(std::fs::read(&target).unwrap(), bytes);

    // The retry: same child, same target, which is now the entry's only external
    // path. Pre-fix this truncated `target` to 0 bytes and reported a LocalFault.
    let err = super::blobs::export_child(&store, tt.hash(), &target)
        .await
        .expect("no local pre-step failure")
        .expect_err("re-exporting onto the entry's own external path cannot succeed");
    assert!(
        super::blobs::export_source_vanished(&err),
        "the re-export degrades to the self-healing vanished class, never a \
         LocalFault that terminalizes the row: {err:?}"
    );
    match std::fs::metadata(&target) {
        Err(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::NotFound,
            "target is either gone or intact — nothing else"
        ),
        Ok(m) => assert_eq!(
            m.len(),
            bytes.len() as u64,
            "the target must never be left truncated"
        ),
    }

    // And the legitimate multi-target path is untouched: a fresh entry exported
    // to two DIFFERENT targets copies from the external path and lands intact.
    let store2 = iroh_blobs::store::fs::FsStore::load(tmp.path().join("s2"))
        .await
        .unwrap();
    let tt2 = store2
        .blobs()
        .add_bytes(bytes.clone())
        .temp_tag()
        .await
        .unwrap();
    let t1 = staging.join("t1.fits");
    let t2 = staging.join("t2.fits");
    super::blobs::export_child(&store2, tt2.hash(), &t1)
        .await
        .unwrap()
        .unwrap();
    super::blobs::export_child(&store2, tt2.hash(), &t2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read(&t1).unwrap(), bytes);
    assert_eq!(std::fs::read(&t2).unwrap(), bytes);
}

// ─── collab v3 wave 2, Task 4: the assignment engine over independent blobs ───
//
// `fetch_items_assigned` carries each item's own `GetRequest`, hash and
// provider list, and reports per item. These tests run real relay-disabled
// nodes with mounted collab stores, served on `COLLAB_BLOBS_ALPN`.

use super::assign::{FailMode, FetchItem};
use super::COLLAB_BLOBS_ALPN;

/// Engine knobs for a raw-item test run: collab ALPN, no hedging, a short
/// stall ceiling so a wedged provider cannot outlast a test's patience.
fn raw_item_opts(
    fail_mode: FailMode,
    total_bytes: u64,
    telemetry: ProviderTelemetrySink,
) -> super::assign::AssignmentOptions {
    super::assign::AssignmentOptions {
        stall_hard_limit: Duration::from_millis(1500),
        hedging: false,
        total_bytes,
        telemetry,
        alpn: COLLAB_BLOBS_ALPN,
        fail_mode,
    }
}

/// A provider node (started `Out`, collab store mounted at `collab_root`)
/// paired both ways with `receiver`.
async fn collab_provider(
    dir: &Path,
    collab_root: &Path,
    receiver: &Arc<SharedIrohNode>,
    receiver_ticket: &str,
) -> (Arc<SharedIrohNode>, NodeId) {
    let node = bind_disabled(dir).await;
    let info = node.handle(Role::Out).start().await.unwrap();
    node.add_peer_ticket(receiver_ticket).unwrap();
    receiver.add_peer_ticket(&info.pairing_ticket).unwrap();
    node.set_collab_root(Some(collab_root))
        .await
        .expect("mount the provider's collab store");
    (node, info.node_id)
}

/// Distinct, non-repeating bytes per `seed`.
fn raw_blob_bytes(seed: usize, len: usize) -> Vec<u8> {
    (0..len).map(|j| ((j + seed * 97) % 251) as u8).collect()
}

/// Add `bytes` to `store` and pin them under `tag`; returns the hash.
async fn seed_raw(store: &Store, bytes: Vec<u8>, tag: &str) -> Hash {
    let tt = store.blobs().add_bytes(bytes).temp_tag().await.unwrap();
    let hash = tt.hash();
    store
        .tags()
        .set(tag, iroh_blobs::HashAndFormat::raw(hash))
        .await
        .unwrap();
    drop(tt);
    hash
}

/// Two providers each hold a DIFFERENT blob; one call fetches both, each from
/// its own provider list, and both succeed. (Driven through the engine's
/// raw-item entry point since its wave-2 collab caller retired, Task 15.)
#[tokio::test]
async fn raw_items_fetch_from_per_item_providers() {
    let (dr, dp1, dp2) = (tempdir().unwrap(), tempdir().unwrap(), tempdir().unwrap());
    let (rr, rp1, rp2) = (tempdir().unwrap(), tempdir().unwrap(), tempdir().unwrap());

    let r = bind_disabled(dr.path()).await;
    let r_info = r.handle(Role::Recv).start().await.unwrap();
    r.set_collab_root(Some(rr.path())).await.unwrap();
    let (p1, p1_id) = collab_provider(dp1.path(), rp1.path(), &r, &r_info.pairing_ticket).await;
    let (p2, p2_id) = collab_provider(dp2.path(), rp2.path(), &r, &r_info.pairing_ticket).await;

    const SIZE: usize = 96 * 1024;
    let h1 = seed_raw(
        &p1.collab_store().unwrap(),
        raw_blob_bytes(1, SIZE),
        "project/p/f1/1",
    )
    .await;
    let h2 = seed_raw(
        &p2.collab_store().unwrap(),
        raw_blob_bytes(2, SIZE),
        "project/p/f2/1",
    )
    .await;

    let r_store = r.collab_store().unwrap();
    let items = vec![
        FetchItem {
            key: "f1".to_string(),
            request: iroh_blobs::protocol::GetRequest::blob(h1),
            hash: h1,
            size: SIZE as u64,
            providers: Arc::new(vec![endpoint_id(p1_id)]),
        },
        FetchItem {
            key: "f2".to_string(),
            request: iroh_blobs::protocol::GetRequest::blob(h2),
            hash: h2,
            size: SIZE as u64,
            providers: Arc::new(vec![endpoint_id(p2_id)]),
        },
    ];
    let (telemetry, _seen) = recording_telemetry();
    let (_report, results) = tokio::time::timeout(
        Duration::from_secs(60),
        super::assign::fetch_items_assigned(
            &r_store,
            &r.endpoint(),
            items,
            raw_item_opts(FailMode::Isolate, 2 * SIZE as u64, telemetry),
        ),
    )
    .await
    .expect("two small blobs must not take a minute")
    .expect("the batch call itself succeeds");

    assert_eq!(results.len(), 2, "one result per item");
    assert_eq!(results[0].0, "f1", "keys are echoed in input order");
    assert_eq!(results[1].0, "f2");
    for (key, res) in &results {
        assert!(
            res.is_ok(),
            "{key} must be fetched from its own provider: {res:?}"
        );
    }
    for h in [h1, h2] {
        assert!(
            r_store.blobs().has(h).await.unwrap(),
            "the receiver's collab store holds {h} complete"
        );
    }

    p1.shutdown().await;
    p2.shutdown().await;
    r.shutdown().await;
}

/// `FailMode::Isolate`: an item whose only provider does not hold its hash
/// fails ALONE — its sibling on another provider still lands.
#[tokio::test]
async fn isolate_mode_keeps_siblings_alive() {
    let (dr, dp1, dp2) = (tempdir().unwrap(), tempdir().unwrap(), tempdir().unwrap());
    let (rr, rp1, rp2) = (tempdir().unwrap(), tempdir().unwrap(), tempdir().unwrap());

    let r = bind_disabled(dr.path()).await;
    let r_info = r.handle(Role::Recv).start().await.unwrap();
    r.set_collab_root(Some(rr.path())).await.unwrap();
    let (p1, p1_id) = collab_provider(dp1.path(), rp1.path(), &r, &r_info.pairing_ticket).await;
    let (p2, p2_id) = collab_provider(dp2.path(), rp2.path(), &r, &r_info.pairing_ticket).await;

    const SIZE: usize = 64 * 1024;
    let absent = Hash::new(raw_blob_bytes(7, SIZE));
    let present = seed_raw(
        &p2.collab_store().unwrap(),
        raw_blob_bytes(8, SIZE),
        "project/p/f2/1",
    )
    .await;

    let items = vec![
        FetchItem {
            key: "missing".to_string(),
            request: GetRequest::blob(absent),
            hash: absent,
            size: SIZE as u64,
            providers: Arc::new(vec![endpoint_id(p1_id)]),
        },
        FetchItem {
            key: "present".to_string(),
            request: GetRequest::blob(present),
            hash: present,
            size: SIZE as u64,
            providers: Arc::new(vec![endpoint_id(p2_id)]),
        },
    ];
    let r_store = r.collab_store().unwrap();
    let (telemetry, _seen) = recording_telemetry();
    let (_report, results) = tokio::time::timeout(
        // The missing item walks the whole backoff ladder (≈ 32 s) before it
        // gives up; see `assigned_fetch_fails_fast_when_every_provider_is_dead`.
        Duration::from_secs(120),
        super::assign::fetch_items_assigned(
            &r_store,
            &r.endpoint(),
            items,
            raw_item_opts(FailMode::Isolate, 2 * SIZE as u64, telemetry),
        ),
    )
    .await
    .expect("an isolated failure must end inside the ladder")
    .expect("isolate mode never fails the whole call");

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, "missing");
    assert!(
        results[0].1.is_err(),
        "an item whose only provider lacks the hash fails: {:?}",
        results[0].1
    );
    assert_eq!(results[1].0, "present");
    assert!(
        results[1].1.is_ok(),
        "its sibling must not be aborted by that failure: {:?}",
        results[1].1
    );
    assert!(r_store.blobs().has(present).await.unwrap());
    assert!(!r_store.blobs().has(absent).await.unwrap());

    p1.shutdown().await;
    p2.shutdown().await;
    r.shutdown().await;
}

/// Hedging on RAW items, over the network (collab v3 wave 2, Task 9 — the
/// Task 4 unit tests covered the raw-item back half only in isolation).
///
/// The collab replication batch runs with `hedging: true`, and a raw item's
/// hedge is `GetRequest::blob_ranges` over the back half of what is still
/// missing. This is `hedge_fires_before_the_stall_ceiling_on_a_slow_provider`
/// rebuilt on raw blobs served from three collab stores: every provider
/// paced the same, then one collapses to 1 KB/s mid-transfer. The items it
/// was serving must be rescued by hedges, not by the 60 s stall ceiling —
/// and every item lands intact.
#[tokio::test]
async fn hedge_fires_on_raw_items_before_the_stall_ceiling() {
    // Same budget arithmetic as the collection test: forty 256 KiB items.
    const ITEMS: usize = 40;
    const SIZE: usize = 256 * 1024;
    let dirs: Vec<_> = (0..7).map(|_| tempdir().unwrap()).collect();
    let r = bind_disabled(dirs[0].path()).await;
    let r_info = r.handle(Role::Recv).start().await.unwrap();
    r.set_collab_root(Some(dirs[1].path())).await.unwrap();
    let (a, a_id) =
        collab_provider(dirs[2].path(), dirs[3].path(), &r, &r_info.pairing_ticket).await;
    let (b, b_id) =
        collab_provider(dirs[4].path(), dirs[5].path(), &r, &r_info.pairing_ticket).await;
    let slow_root = dirs[6].path().join("collab");
    std::fs::create_dir_all(&slow_root).unwrap();
    let (s, s_id) = collab_provider(
        &dirs[6].path().join("node"),
        &slow_root,
        &r,
        &r_info.pairing_ticket,
    )
    .await;

    let mut hashes = Vec::with_capacity(ITEMS);
    for i in 0..ITEMS {
        let bytes = raw_blob_bytes(100 + i, SIZE);
        let mut h = None;
        for p in [&a, &b, &s] {
            h = Some(
                seed_raw(
                    &p.collab_store().unwrap(),
                    bytes.clone(),
                    &format!("project/p/f{i}/1"),
                )
                .await,
            );
        }
        hashes.push(h.unwrap());
    }
    for p in [&a, &b, &s] {
        p.set_upload_limit(400_000);
    }
    let providers = Arc::new(vec![
        endpoint_id(a_id),
        endpoint_id(b_id),
        endpoint_id(s_id),
    ]);
    let items: Vec<FetchItem> = hashes
        .iter()
        .enumerate()
        .map(|(i, h)| FetchItem {
            key: format!("f{i}"),
            request: GetRequest::blob(*h),
            hash: *h,
            size: SIZE as u64,
            providers: Arc::clone(&providers),
        })
        .collect();
    let (telemetry, _) = recording_telemetry();
    let opts = super::assign::AssignmentOptions {
        stall_hard_limit: Duration::from_secs(60),
        hedging: true,
        total_bytes: (ITEMS * SIZE) as u64,
        telemetry,
        alpn: COLLAB_BLOBS_ALPN,
        fail_mode: FailMode::Isolate,
    };
    let collapse = {
        let slow = Arc::clone(&s);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            slow.set_upload_limit(1_000);
        })
    };
    let r_store = r.collab_store().unwrap();
    let started = Instant::now();
    let (report, results) = tokio::time::timeout(
        Duration::from_secs(120),
        super::assign::fetch_items_assigned(&r_store, &r.endpoint(), items, opts),
    )
    .await
    .expect("a hedged raw fetch must not wait out the stall ceiling")
    .expect("the assigned loop always reports");
    let elapsed = started.elapsed();
    collapse.await.ok();

    for (key, res) in &results {
        assert!(res.is_ok(), "{key}: {res:?}");
    }
    for h in &hashes {
        assert!(
            r_store.blobs().has(*h).await.unwrap(),
            "{h} landed complete"
        );
    }
    assert!(
        elapsed < Duration::from_secs(45),
        "hedging must finish before the 60 s ceiling could — took {elapsed:?} ({report:?})"
    );
    assert!(
        report.hedges >= 1,
        "a raw item must have been hedged: {report:?}"
    );
    assert_eq!(
        report.stalls, 0,
        "the ceiling must not be what rescued it: {report:?}"
    );

    for n in [a, b, s, r] {
        n.shutdown().await;
    }
}

/// The collection caller keeps `FailMode::FailFast`: one child the provider
/// cannot serve fails the WHOLE call, exactly as before the generalization.
#[tokio::test]
async fn fail_fast_still_applies_to_collections() {
    let (dp, dr) = (tempdir().unwrap(), tempdir().unwrap());
    let p = bind_disabled(dp.path()).await;
    let r = bind_disabled(dr.path()).await;
    let p_info = p.handle(Role::Out).start().await.unwrap();
    let r_info = r.handle(Role::Recv).start().await.unwrap();
    r.add_peer_ticket(&p_info.pairing_ticket).unwrap();
    p.add_peer_ticket(&r_info.pairing_ticket).unwrap();

    // A hash sequence [held, missing] on the provider's personal store, which
    // `iroh_blobs::ALPN` serves. The receiver needs the sequence itself
    // locally to address children through it (phase 1's job in production).
    let held = seed_raw(p.store(), raw_blob_bytes(3, 64 * 1024), "t/held").await;
    let missing = Hash::new(raw_blob_bytes(4, 64 * 1024));
    let seq: iroh_blobs::hashseq::HashSeq = [held, missing].into_iter().collect();
    let seq_bytes = seq.into_inner();
    let root = seed_raw(p.store(), seq_bytes.to_vec(), "t/root").await;
    assert_eq!(
        seed_raw(r.store(), seq_bytes.to_vec(), "t/root").await,
        root
    );

    let (telemetry, _seen) = recording_telemetry();
    let res = tokio::time::timeout(
        Duration::from_secs(120),
        super::assign::fetch_children_assigned(
            r.store(),
            &r.endpoint(),
            vec![endpoint_id(p_info.node_id)],
            root,
            vec![(0, held), (1, missing)],
            // The wrapper forces the personal ALPN and fail-fast whatever
            // these say.
            raw_item_opts(FailMode::Isolate, 128 * 1024, telemetry),
        ),
    )
    .await
    .expect("a missing child must fail inside the ladder, never hang");
    assert!(
        res.is_err(),
        "a collection with one unservable child fails as a whole: {res:?}"
    );

    p.shutdown().await;
    r.shutdown().await;
}

/// The hedge's back-half request: a raw item asks for `blob_ranges` of its own
/// hash; a collection child keeps the old `child(i).build(root)` shape.
#[test]
fn raw_item_hedge_uses_blob_ranges() {
    let h = Hash::new(b"raw frame");
    let root = Hash::new(b"hash seq root");
    let back = ChunkRanges::chunks(32u64..);

    let raw = super::assign::hedge_back_half_request(&GetRequest::blob(h), h, back.clone());
    assert_eq!(raw, Some(GetRequest::blob_ranges(h, back.clone())));

    for index in [0u64, 1, 7] {
        let child = GetRequest::builder()
            .child(index, ChunkRanges::all())
            .build(root);
        let got = super::assign::hedge_back_half_request(&child, h, back.clone());
        assert_eq!(
            got,
            Some(GetRequest::builder().child(index, back.clone()).build(root)),
            "child {index} keeps the collection builder output"
        );
    }
}

/// One `fetch_items_assigned` call opens ONE connection pool, however many
/// items it carries.
#[tokio::test]
async fn one_pool_per_call() {
    let (dr, dp, rr, rp) = (
        tempdir().unwrap(),
        tempdir().unwrap(),
        tempdir().unwrap(),
        tempdir().unwrap(),
    );
    let r = bind_disabled(dr.path()).await;
    let r_info = r.handle(Role::Recv).start().await.unwrap();
    r.set_collab_root(Some(rr.path())).await.unwrap();
    let (p, p_id) = collab_provider(dp.path(), rp.path(), &r, &r_info.pairing_ticket).await;

    const ITEMS: usize = 20;
    const SIZE: usize = 8 * 1024;
    let p_store = p.collab_store().unwrap();
    let providers = Arc::new(vec![endpoint_id(p_id)]);
    let mut items = Vec::with_capacity(ITEMS);
    for i in 0..ITEMS {
        let hash = seed_raw(
            &p_store,
            raw_blob_bytes(100 + i, SIZE),
            &format!("project/p/{i}/1"),
        )
        .await;
        items.push(FetchItem {
            key: format!("frame-{i:02}"),
            request: GetRequest::blob(hash),
            hash,
            size: SIZE as u64,
            providers: Arc::clone(&providers),
        });
    }

    let before = super::assign::pools_opened_on_this_thread();
    let (telemetry, _seen) = recording_telemetry();
    let (_report, results) = tokio::time::timeout(
        Duration::from_secs(60),
        super::assign::fetch_items_assigned(
            &r.collab_store().unwrap(),
            &r.endpoint(),
            items,
            raw_item_opts(FailMode::Isolate, (ITEMS * SIZE) as u64, telemetry),
        ),
    )
    .await
    .expect("twenty small blobs must not take a minute")
    .expect("the batch call succeeds");
    assert_eq!(
        super::assign::pools_opened_on_this_thread() - before,
        1,
        "one call opens exactly one pool"
    );
    assert_eq!(results.len(), ITEMS);
    assert!(results.iter().all(|(_, r)| r.is_ok()), "{results:?}");

    p.shutdown().await;
    r.shutdown().await;
}

// ─── collab v3 wave 3, Task 12: the live assignment run ───────────────────
//
// `run_live` over the collab pool against signed-in providers with serve
// oracles (the landed rigs). Gated like the fixtures they use (P1 headless
// rule): `api::collab_live::test_support` needs `render` + `solver`.

#[cfg(all(feature = "render", feature = "solver"))]
mod live_run {
    use std::sync::Arc;
    use std::time::Duration;

    use iroh_blobs::api::proto::BlobStatus;

    use crate::api::collab_live::test_support as ts;
    use crate::db::collab_frames::LocalState;
    use crate::sharing::iroh::assign::{
        is_refused_by_every_provider, Dialer, ItemOutcome, LiveVerdict, ProviderSet,
    };

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_live_item_waits_for_its_first_provider_then_completes() {
        let rig = ts::landed_rig(1).await;
        let me = ts::bare_node().await;
        ts::pair(&me, &rig.node).await;
        let store = ts::scratch_store();
        let provider = rig.node.endpoint_addr().id;
        let (prov_tx, prov_rx) =
            tokio::sync::watch::channel(Arc::new(Vec::<iroh::EndpointId>::new()));
        let (out, report) = ts::run_one_live(
            &me,
            &store,
            &rig,
            0,
            ProviderSet::Live(prov_rx),
            async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                prov_tx.send(Arc::new(vec![provider])).unwrap(); // a provider appears
            },
        )
        .await;
        assert!(matches!(out, ItemOutcome::Done), "{out:?}");
        assert!(report.total_bytes() > 0);
        assert!(store.blobs().has(rig.hash_of(0)).await.unwrap());
    }

    /// Task 15 (T12 carry): raising the stream limit takes a queued item at
    /// once — the run wakes on the limit's watch instead of waiting for its
    /// next join, item or yield.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn raising_the_stream_limit_takes_a_queued_item_at_once() {
        let rig = ts::landed_rig(2).await;
        let me = ts::bare_node().await;
        ts::pair(&me, &rig.node).await;
        let store = ts::scratch_store();
        let provider = rig.node.endpoint_addr().id;
        // item 0 waits forever on an empty live set; item 1 could run
        let (_empty_tx, empty_rx) =
            tokio::sync::watch::channel(Arc::new(Vec::<iroh::EndpointId>::new()));
        let (waiting, _c0) = ts::rig_item(&rig, 0, ProviderSet::Live(empty_rx));
        let (ready, _c1) = ts::rig_item(&rig, 1, ProviderSet::Fixed(Arc::new(vec![provider])));
        let (item_tx, item_rx) = tokio::sync::mpsc::channel(4);
        item_tx.send(waiting).await.unwrap();
        item_tx.send(ready).await.unwrap();
        let mut opts = ts::live_opts(1, crate::sharing::noop_provider_telemetry());
        let limit = Arc::clone(&opts.max_in_flight);
        let (limit_tx, limit_rx) = tokio::sync::watch::channel(1usize);
        opts.limit_changed = Some(limit_rx);
        let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel();
        let (verdict_tx, _verdicts) = tokio::sync::mpsc::unbounded_channel();
        let (_yield_tx, yield_rx) = tokio::sync::watch::channel(false);
        let dialer = ts::live_dialer(&me, &[&rig.node]);
        let run = tokio::spawn({
            let store = store.clone();
            async move {
                crate::sharing::iroh::assign::run_live(
                    &store, dialer, item_rx, opts, done_tx, verdict_tx, yield_rx,
                )
                .await
            }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            done_rx.try_recv().is_err(),
            "one slot, held by the waiting item"
        );
        limit.store(2, std::sync::atomic::Ordering::Relaxed);
        limit_tx.send(2).unwrap();
        let (key, out) = tokio::time::timeout(Duration::from_secs(10), done_rx.recv())
            .await
            .expect("the raised limit took the queued item at once")
            .unwrap();
        assert_eq!(key, "f01");
        assert!(matches!(out, ItemOutcome::Done), "{out:?}");
        run.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refusing_provider_is_excluded_for_the_hash_without_a_strike() {
        // provider A edited its file (serve check refuses: ERR_PERMISSION), B serves
        let (a, b) = ts::two_landed_providers().await;
        ts::overwrite_same_size(&a.frames[0].2);
        let me = ts::bare_node().await;
        ts::pair(&me, &a.node).await;
        ts::pair(&me, &b.node).await;
        let store = ts::scratch_store();
        let (out, verdicts) =
            ts::run_one_live_fixed(&me, &store, a.hash_of(0), a.frames[0].1.clone(), &[&a, &b])
                .await;
        assert!(matches!(out, ItemOutcome::Done), "{out:?}");
        let a_id = a.node.endpoint_addr().id;
        assert!(
            verdicts
                .iter()
                .any(|v| matches!(v, LiveVerdict::Refused { provider, .. } if *provider == a_id)),
            "{verdicts:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_busy_provider_is_retried_later_and_another_one_serves_now() {
        let (a, b) = ts::two_landed_providers().await;
        a.node.set_collab_upload_limit(1);
        let _busy = a.node.collab_stream_gauge_for_test().try_acquire().unwrap();
        let me = ts::bare_node().await;
        ts::pair(&me, &a.node).await;
        ts::pair(&me, &b.node).await;
        let store = ts::scratch_store();
        let (out, verdicts) =
            ts::run_one_live_fixed(&me, &store, a.hash_of(0), a.frames[0].1.clone(), &[&a, &b])
                .await;
        assert!(matches!(out, ItemOutcome::Done), "{out:?}");
        let a_id = a.node.endpoint_addr().id;
        assert!(
            verdicts
                .iter()
                .any(|v| matches!(v, LiveVerdict::Busy { provider } if *provider == a_id)),
            "{verdicts:?}"
        );
    }

    /// With only a busy provider, the item waits for a free stream — no
    /// strike, no exhausted ladder — and is served once one frees up.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lone_busy_provider_serves_once_a_stream_frees_up() {
        let rig = ts::landed_rig(1).await;
        rig.node.set_collab_upload_limit(1);
        let busy = rig
            .node
            .collab_stream_gauge_for_test()
            .try_acquire()
            .unwrap();
        let me = ts::bare_node().await;
        ts::pair(&me, &rig.node).await;
        let store = ts::scratch_store();
        let (out, _report) = ts::run_one_live(
            &me,
            &store,
            &rig,
            0,
            ProviderSet::Fixed(Arc::new(vec![rig.node.endpoint_addr().id])),
            async move {
                // Longer than the whole failure ladder would allow for
                // six busy refusals if each spent a round.
                tokio::time::sleep(Duration::from_secs(5)).await;
                drop(busy);
            },
        )
        .await;
        assert!(matches!(out, ItemOutcome::Done), "{out:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_stops_one_item_and_keeps_its_partial_bytes() {
        let rig = ts::landed_rig_big(1, 64 * 1024 * 1024).await; // one 64 MiB frame
        rig.node.set_upload_limit(8 * 1024 * 1024); // slow it down: 8 MB/s
        let me = ts::bare_node().await;
        ts::pair(&me, &rig.node).await;
        let store = ts::scratch_store();
        let out = ts::run_one_live_cancel_once_moving(&me, &store, &rig, 0).await;
        assert!(matches!(out, ItemOutcome::Cancelled), "{out:?}");
        assert!(matches!(
            store.blobs().status(rig.hash_of(0)).await.unwrap(),
            BlobStatus::Partial { .. }
        ));
    }

    /// T12 ruling R4 (replaces the retired wave-2 resume test): a fetch cut
    /// off mid-transfer — cancelled, then its provider gone — resumes from
    /// its verified ranges on a second run against another provider holding
    /// the same bytes, moving fewer bytes than the whole frame.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_live_fetch_resumes_from_partial_bytes_after_the_provider_drops() {
        const SIZE: usize = 64 * 1024 * 1024;
        let a = ts::landed_rig_big(1, SIZE).await;
        let b = ts::landed_rig_big(1, SIZE).await;
        let hash = a.hash_of(0);
        assert_eq!(hash, b.hash_of(0), "the same frame on both providers");
        a.node.set_upload_limit(8 * 1024 * 1024);
        let me = ts::bare_node().await;
        ts::pair(&me, &a.node).await;
        ts::pair(&me, &b.node).await;
        let store = ts::scratch_store();

        let out = ts::run_one_live_cancel_once_moving(&me, &store, &a, 0).await;
        assert!(matches!(out, ItemOutcome::Cancelled), "{out:?}");
        assert!(matches!(
            store.blobs().status(hash).await.unwrap(),
            BlobStatus::Partial { .. }
        ));
        a.node.shutdown().await; // the provider drops

        let (out, report) = ts::run_one_live(
            &me,
            &store,
            &b,
            0,
            ProviderSet::Fixed(Arc::new(vec![b.node.endpoint_addr().id])),
            async {},
        )
        .await;
        assert!(matches!(out, ItemOutcome::Done), "{out:?}");
        assert!(matches!(
            store.blobs().status(hash).await.unwrap(),
            BlobStatus::Complete { .. }
        ));
        let moved = report.total_bytes();
        assert!(
            moved > 0 && moved < SIZE as u64,
            "the second run moved only the missing ranges: {moved} of {SIZE} ({report:?})"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_stream_cap_bounds_concurrent_items_and_yield_stops_taking_new_ones() {
        let rig = ts::landed_rig(4).await;
        let me = ts::bare_node().await;
        ts::pair(&me, &rig.node).await;
        let store = ts::scratch_store();
        let stats = ts::run_many_live(
            &me, &store, &rig, /*max_in_flight*/ 1, /*yield after first*/ true,
        )
        .await;
        assert_eq!(stats.max_concurrent, 1);
        assert_eq!(
            stats.completed, 1,
            "yield: the in-flight item finished, the queued ones were not taken"
        );
        assert!(stats.returned_early);
    }

    /// Without a yield the cap still holds and every queued item completes;
    /// the run returns once the closed channel is drained.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_stream_cap_holds_across_a_whole_queue() {
        let rig = ts::landed_rig(4).await;
        let me = ts::bare_node().await;
        ts::pair(&me, &rig.node).await;
        let store = ts::scratch_store();
        let stats = ts::run_many_live(&me, &store, &rig, 2, false).await;
        assert!(stats.max_concurrent <= 2, "{}", stats.max_concurrent);
        assert_eq!(stats.completed, 4);
        assert!(!stats.returned_early);
    }

    // ─── fix round 1 ─────────────────────────────────────────────────────

    /// I1: an item waiting on an empty live set is not transferring, so a
    /// yield cuts it at once — it does not hold its slot until a provider
    /// appears.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_waiting_item_is_cut_at_once_by_a_yield() {
        let me = ts::bare_node().await;
        let store = ts::scratch_store();
        let (_prov_tx, prov_rx) =
            tokio::sync::watch::channel(Arc::new(Vec::<iroh::EndpointId>::new()));
        let hash = iroh_blobs::Hash::new(b"a frame nobody holds yet");
        let (item, _cancel) = ts::live_item("f00", hash, 1024, ProviderSet::Live(prov_rx));
        let (outcomes, after_yield) = ts::run_live_then_yield(
            &store,
            ts::live_dialer(&me, &[]),
            vec![item],
            Duration::from_millis(300),
        )
        .await;
        assert!(
            after_yield < Duration::from_secs(2),
            "the run returned {after_yield:?} after the yield"
        );
        assert_eq!(outcomes.len(), 1);
        assert!(
            matches!(outcomes[0].1, ItemOutcome::Cancelled),
            "{outcomes:?}"
        );
    }

    /// I2 (a): a provider that refused, left the live set and came back is
    /// asked again — its exclusion ended when it was seen absent.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_provider_that_leaves_and_returns_is_asked_again() {
        let (a, b) = ts::two_landed_providers().await;
        // A refuses (its row is not held); B is busy, so the item waits.
        ts::set_frame_state_raw(&a, 0, LocalState::Idle);
        b.node.set_collab_upload_limit(1);
        let busy = b.node.collab_stream_gauge_for_test().try_acquire().unwrap();
        let me = ts::bare_node().await;
        ts::pair(&me, &a.node).await;
        ts::pair(&me, &b.node).await;
        let store = ts::scratch_store();
        let (a_id, b_id) = (a.node.endpoint_addr().id, b.node.endpoint_addr().id);
        let (prov_tx, prov_rx) = tokio::sync::watch::channel(Arc::new(vec![a_id, b_id]));
        let (item, _cancel) =
            ts::live_item(&a.frames[0].1, a.hash_of(0), 0, ProviderSet::Live(prov_rx));
        let run = ts::drive_live(
            &store,
            ts::live_dialer(&me, &[&a.node, &b.node]),
            vec![item],
            ts::live_opts(8, crate::sharing::noop_provider_telemetry()),
            async {
                tokio::time::sleep(Duration::from_millis(800)).await;
                prov_tx.send_replace(Arc::new(vec![b_id])); // A leaves
                tokio::time::sleep(Duration::from_millis(300)).await;
                ts::set_frame_state_raw(&a, 0, LocalState::Held);
                prov_tx.send_replace(Arc::new(vec![a_id, b_id])); // A comes back
            },
        )
        .await;
        drop(busy);
        assert_eq!(run.outcomes.len(), 1);
        assert!(
            matches!(run.outcomes[0].1, ItemOutcome::Done),
            "{:?}",
            run.outcomes
        );
        assert!(
            run.verdicts
                .iter()
                .any(|v| matches!(v, LiveVerdict::Refused { provider, .. } if *provider == a_id)),
            "{:?}",
            run.verdicts
        );
        assert!(
            run.report
                .per_provider
                .get(&a_id)
                .is_some_and(|s| s.bytes > 0),
            "A served it after coming back: {:?}",
            run.report
        );
    }

    /// I2 (b): a live item whose every provider refused it fails at once,
    /// with a cause the scheduler recognises.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_live_item_every_provider_refused_fails_to_the_scheduler() {
        let rig = ts::landed_rig(1).await;
        ts::set_frame_state_raw(&rig, 0, LocalState::Idle);
        let me = ts::bare_node().await;
        ts::pair(&me, &rig.node).await;
        let store = ts::scratch_store();
        let (_prov_tx, prov_rx) =
            tokio::sync::watch::channel(Arc::new(vec![rig.node.endpoint_addr().id]));
        let started = std::time::Instant::now();
        let (out, _report) =
            ts::run_one_live(&me, &store, &rig, 0, ProviderSet::Live(prov_rx), async {}).await;
        assert!(started.elapsed() < Duration::from_secs(5));
        let ItemOutcome::Failed(e) = out else {
            panic!("expected Failed, got {out:?}");
        };
        assert!(is_refused_by_every_provider(&e), "{e:#}");
        assert!(
            format!("{e:#}").starts_with("refused by every provider"),
            "{e:#}"
        );
    }

    /// I3: set changes that wake an item early from a failure backoff spend
    /// no round — ten of them, more than the whole ladder, do not fail it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn set_changes_during_a_backoff_spend_no_round() {
        let rig = ts::landed_rig(1).await;
        let me = ts::bare_node().await;
        ts::pair(&me, &rig.node).await;
        let store = ts::scratch_store();
        let provider = rig.node.endpoint_addr().id;
        // No address at first: every dial fails, the provider is struck into
        // backoff after three.
        let book: Arc<std::sync::Mutex<Option<iroh::EndpointAddr>>> =
            Arc::new(std::sync::Mutex::new(None));
        let dialer = {
            let book = Arc::clone(&book);
            let (events, _) = tokio::sync::mpsc::unbounded_channel();
            Dialer::Collab {
                pool: crate::sharing::iroh::collab_pool::CollabPool::new(me.endpoint(), events),
                addrs: Arc::new(move |_| book.lock().unwrap().clone()),
            }
        };
        let (prov_tx, prov_rx) = tokio::sync::watch::channel(Arc::new(vec![provider]));
        let (item, _cancel) = ts::live_item(
            &rig.frames[0].1,
            rig.hash_of(0),
            0,
            ProviderSet::Live(prov_rx),
        );
        let addr = rig.node.endpoint_addr();
        let run = ts::drive_live(
            &store,
            dialer,
            vec![item],
            ts::live_opts(8, crate::sharing::noop_provider_telemetry()),
            async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                for _ in 0..10 {
                    prov_tx.send_replace(Arc::new(vec![provider]));
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                *book.lock().unwrap() = Some(addr);
            },
        )
        .await;
        assert_eq!(run.outcomes.len(), 1);
        assert!(
            matches!(run.outcomes[0].1, ItemOutcome::Done),
            "{:?}",
            run.outcomes
        );
        assert!(
            run.verdicts
                .iter()
                .filter(|v| matches!(v, LiveVerdict::DialFailed { .. }))
                .count()
                >= 3,
            "the provider was struck into backoff first: {:?}",
            run.verdicts
        );
    }
}
