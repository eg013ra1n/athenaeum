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

use super::assign::{AssignmentReport, ProviderStats};
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
            exclude_collab_landings: false,
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
            exclude_collab_landings: false,
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
/// the regression it guards against (one provider serving everything): the
/// serving provider sent 1_430_494 B, the
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

/// Write an N-payload package and return `(pkg_dir, announce)`.
///
/// A package with MANY collection children, so that spreading work over several
/// providers is observable rather than a coin flip.
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

/// A [`StartInfo`]-shaped node id as the assignment report keys it.
fn endpoint_id(id: NodeId) -> EndpointId {
    EndpointId::from_bytes(&id).expect("a node id from StartInfo is a valid endpoint id")
}

// ---------------------------------------------------------------------------
// A2b Task 8 Step 1 — the HARD GATE for hedging.
//
// A hedge is a second transfer of part of a blob another transfer is already
// fetching. Hedging is pointless unless the store tolerates two
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

// ─── the assignment engine over independent blobs ─────────────────────────
//
// These tests run real relay-disabled nodes with mounted collab stores, served
// on `COLLAB_BLOBS_ALPN`.

use super::assign::FetchItem;

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

/// The hedge's back-half request for a collection-child request keeps the
/// `child(i).build(root)` shape (the raw-blob shape is pinned by
/// `live_raw_item_hedge_uses_blob_ranges`).
#[test]
fn hedge_back_half_of_a_child_request_keeps_the_child_shape() {
    let h = Hash::new(b"raw frame");
    let root = Hash::new(b"hash seq root");
    let back = ChunkRanges::chunks(32u64..);

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

// ─── the shared fetch engine, entered through the live run ────────────────
//
// `run_live` is the one entry point production uses. These tests pin the
// engine's behaviours — hedging, the stall ceiling, provider failover and
// attribution, per-item provider lists, isolation — by feeding raw blobs on
// collab providers through it.

use super::assign::{
    run_live, Dialer, ItemOutcome, LiveItem, LiveRunOptions, LiveVerdict, ProviderSet,
};

/// A collab-pool dialer from `me` whose address book is `providers`. The pool's
/// connect timeout is the stock pool's 1 s, so a dead provider does not wait out
/// the production 10 s per rung of the backoff ladder.
fn live_collab_dialer(me: &SharedIrohNode, providers: &[&Arc<SharedIrohNode>]) -> Dialer {
    let book: HashMap<EndpointId, iroh::EndpointAddr> = providers
        .iter()
        .map(|p| {
            let addr = p.endpoint_addr();
            (addr.id, addr)
        })
        .collect();
    let (events, _) = tokio::sync::mpsc::unbounded_channel();
    Dialer {
        pool: super::collab_pool::CollabPool::with_connect_timeout(
            me.endpoint(),
            events,
            Duration::from_secs(1),
        ),
        addrs: Arc::new(move |id| book.get(id).cloned()),
    }
}

/// Live-run knobs: the given stall ceiling and hedging, `max_in_flight` items at
/// once, no yield cap.
fn live_run_opts(
    stall_hard_limit: Duration,
    hedging: bool,
    max_in_flight: usize,
    telemetry: ProviderTelemetrySink,
) -> LiveRunOptions {
    LiveRunOptions {
        stall_hard_limit,
        hedging,
        telemetry,
        max_in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(max_in_flight)),
        limit_changed: None,
        unit_cap_bytes: u64::MAX,
        delivered: None,
    }
}

/// One raw-blob live item on a fixed provider list. Its cancel switch is never
/// flipped (the sender is dropped, which never fires it).
fn live_blob_item(key: &str, hash: Hash, size: u64, providers: &[EndpointId]) -> LiveItem {
    let (_cancel_tx, cancel) = tokio::sync::watch::channel(false);
    LiveItem {
        item: FetchItem {
            key: key.to_string(),
            request: GetRequest::blob(hash),
            hash,
            size,
            providers: ProviderSet::Fixed(Arc::new(providers.to_vec())),
        },
        cancel,
    }
}

/// What one `run_live` produced: every outcome (in completion order), every
/// verdict, and the engine's report.
struct LiveOut {
    outcomes: Vec<(String, ItemOutcome)>,
    verdicts: Vec<LiveVerdict>,
    report: AssignmentReport,
}

impl LiveOut {
    fn outcome(&self, key: &str) -> &ItemOutcome {
        &self
            .outcomes
            .iter()
            .find(|(k, _)| k == key)
            .unwrap_or_else(|| panic!("no outcome for {key}: {:?}", self.outcomes))
            .1
    }

    /// One outcome per item, every one `Done`.
    fn assert_all_done(&self, items: usize) {
        assert_eq!(self.outcomes.len(), items, "one outcome per item");
        for (key, out) in &self.outcomes {
            assert!(matches!(out, ItemOutcome::Done), "{key}: {out:?}");
        }
    }
}

/// Feed `items` into `run_live` (the item channel closed behind them) and
/// collect everything it reports, within `patience`. Dropping the run on a
/// timeout cancels it cleanly.
async fn run_live_items(
    store: &Store,
    dialer: Dialer,
    items: Vec<LiveItem>,
    opts: LiveRunOptions,
    patience: Duration,
) -> LiveOut {
    let real = items.len();
    let (item_tx, item_rx) = tokio::sync::mpsc::channel(real.max(1));
    for item in items {
        item_tx.send(item).await.unwrap();
    }
    drop(item_tx);
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel();
    let (verdict_tx, mut verdict_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_yield_tx, yield_rx) = tokio::sync::watch::channel(false);
    let run = run_live(store, dialer, item_rx, opts, done_tx, verdict_tx, yield_rx);
    // Outcomes arrive as items end; the channel closes when the run returns.
    let collect = async {
        let mut outcomes = Vec::new();
        while let Some(done) = done_rx.recv().await {
            outcomes.push(done);
        }
        outcomes
    };
    let (report, outcomes) = tokio::time::timeout(patience, async { tokio::join!(run, collect) })
        .await
        .expect("the live run must end inside the test's patience");
    let mut verdicts = Vec::new();
    while let Ok(v) = verdict_rx.try_recv() {
        verdicts.push(v);
    }
    LiveOut {
        outcomes,
        verdicts,
        report,
    }
}

/// A receiver and N collab providers, paired, each provider holding the same
/// `items` raw blobs of `size` bytes (`f0` .. `f{items-1}`).
struct LiveSwarm {
    recv: Arc<SharedIrohNode>,
    nodes: Vec<Arc<SharedIrohNode>>,
    ids: Vec<EndpointId>,
    hashes: Vec<Hash>,
    size: usize,
    _dirs: Vec<tempfile::TempDir>,
}

async fn live_swarm(providers: usize, items: usize, size: usize) -> LiveSwarm {
    let (recv_dir, recv_root) = (tempdir().unwrap(), tempdir().unwrap());
    let recv = bind_disabled(recv_dir.path()).await;
    let info = recv.handle(Role::Recv).start().await.unwrap();
    recv.set_collab_root(Some(recv_root.path())).await.unwrap();
    let mut dirs = vec![recv_dir, recv_root];
    let (mut nodes, mut ids) = (Vec::new(), Vec::new());
    for _ in 0..providers {
        let d = tempdir().unwrap();
        let root = d.path().join("collab");
        std::fs::create_dir_all(&root).unwrap();
        let (node, id) =
            collab_provider(&d.path().join("node"), &root, &recv, &info.pairing_ticket).await;
        dirs.push(d);
        nodes.push(node);
        ids.push(endpoint_id(id));
    }
    let mut hashes = Vec::with_capacity(items);
    for i in 0..items {
        let bytes = raw_blob_bytes(100 + i, size);
        let mut h = None;
        for n in &nodes {
            h = Some(
                seed_raw(
                    &n.collab_store().unwrap(),
                    bytes.clone(),
                    &format!("project/p/f{i}/1"),
                )
                .await,
            );
        }
        hashes.push(h.unwrap());
    }
    LiveSwarm {
        recv,
        nodes,
        ids,
        hashes,
        size,
        _dirs: dirs,
    }
}

impl LiveSwarm {
    /// Every blob as a live item over every provider.
    fn items(&self) -> Vec<LiveItem> {
        self.hashes
            .iter()
            .enumerate()
            .map(|(i, h)| live_blob_item(&format!("f{i}"), *h, self.size as u64, &self.ids))
            .collect()
    }

    fn dialer(&self) -> Dialer {
        let nodes: Vec<&Arc<SharedIrohNode>> = self.nodes.iter().collect();
        live_collab_dialer(&self.recv, &nodes)
    }

    fn store(&self) -> Store {
        self.recv.collab_store().unwrap()
    }

    fn total(&self) -> u64 {
        (self.hashes.len() * self.size) as u64
    }

    async fn assert_landed(&self) {
        let store = self.store();
        for h in &self.hashes {
            assert!(store.blobs().has(*h).await.unwrap(), "{h} landed complete");
        }
    }

    async fn shutdown(self) {
        for n in self.nodes {
            n.shutdown().await;
        }
        self.recv.shutdown().await;
    }
}

/// A provider's stats, or the empty default: a live run keeps state only for
/// providers it actually claimed, so a never-tried one is absent from the report.
fn live_stats(report: &AssignmentReport, id: EndpointId) -> ProviderStats {
    report.per_provider.get(&id).cloned().unwrap_or_default()
}

/// Two providers each hold a DIFFERENT blob; one run fetches both, each from its
/// own provider list. The live
/// run reports in completion order, so outcomes are looked up by key rather than
/// asserted in input order.
#[tokio::test]
async fn live_items_fetch_from_per_item_providers() {
    const SIZE: usize = 96 * 1024;
    let swarm = live_swarm(2, 0, SIZE).await;
    let h1 = seed_raw(
        &swarm.nodes[0].collab_store().unwrap(),
        raw_blob_bytes(1, SIZE),
        "project/p/f1/1",
    )
    .await;
    let h2 = seed_raw(
        &swarm.nodes[1].collab_store().unwrap(),
        raw_blob_bytes(2, SIZE),
        "project/p/f2/1",
    )
    .await;
    let items = vec![
        live_blob_item("f1", h1, SIZE as u64, &swarm.ids[..1]),
        live_blob_item("f2", h2, SIZE as u64, &swarm.ids[1..]),
    ];
    let (telemetry, _seen) = recording_telemetry();
    let out = run_live_items(
        &swarm.store(),
        swarm.dialer(),
        items,
        live_run_opts(Duration::from_millis(1500), false, 8, telemetry),
        Duration::from_secs(60),
    )
    .await;
    out.assert_all_done(2);
    for h in [h1, h2] {
        assert!(
            swarm.store().blobs().has(h).await.unwrap(),
            "the receiver's collab store holds {h} complete"
        );
    }
    // Each blob came from the one provider that holds it.
    assert!(live_stats(&out.report, swarm.ids[0]).bytes >= SIZE as u64);
    assert!(live_stats(&out.report, swarm.ids[1]).bytes >= SIZE as u64);
    swarm.shutdown().await;
}

/// An item whose only provider does not hold its hash fails ALONE; its sibling
/// on another provider still lands (a live run is always isolated).
#[tokio::test]
async fn live_one_failed_item_keeps_its_siblings_alive() {
    const SIZE: usize = 64 * 1024;
    let swarm = live_swarm(2, 0, SIZE).await;
    let absent = Hash::new(raw_blob_bytes(7, SIZE));
    let present = seed_raw(
        &swarm.nodes[1].collab_store().unwrap(),
        raw_blob_bytes(8, SIZE),
        "project/p/f2/1",
    )
    .await;
    let items = vec![
        live_blob_item("missing", absent, SIZE as u64, &swarm.ids[..1]),
        live_blob_item("present", present, SIZE as u64, &swarm.ids[1..]),
    ];
    let (telemetry, _seen) = recording_telemetry();
    let out = run_live_items(
        &swarm.store(),
        swarm.dialer(),
        items,
        live_run_opts(Duration::from_millis(1500), false, 8, telemetry),
        // The missing item may walk the whole backoff ladder (about 32 s)
        // before it gives up.
        Duration::from_secs(120),
    )
    .await;
    assert_eq!(out.outcomes.len(), 2);
    assert!(
        matches!(out.outcome("missing"), ItemOutcome::Failed(_)),
        "an item whose only provider lacks the hash fails: {:?}",
        out.outcomes
    );
    assert!(
        matches!(out.outcome("present"), ItemOutcome::Done),
        "its sibling must not be aborted by that failure: {:?}",
        out.outcomes
    );
    assert!(swarm.store().blobs().has(present).await.unwrap());
    assert!(!swarm.store().blobs().has(absent).await.unwrap());
    swarm.shutdown().await;
}

/// The hedge's back-half request is a blob-range of the raw item's own hash
/// (the only shape the live run builds).
#[test]
fn live_raw_item_hedge_uses_blob_ranges() {
    let h = Hash::new(b"raw frame");
    let back = ChunkRanges::chunks(32u64..);
    let raw = super::assign::hedge_back_half_request(&GetRequest::blob(h), h, back.clone());
    assert_eq!(raw, Some(GetRequest::blob_ranges(h, back)));
}

/// A run opens ONE collab connection per provider, however many items it carries
/// (the caller owns the pool, so the count to pin is its dials).
#[tokio::test]
async fn live_one_connection_per_provider_for_many_items() {
    const ITEMS: usize = 20;
    const SIZE: usize = 8 * 1024;
    let swarm = live_swarm(1, ITEMS, SIZE).await;
    let dialer = swarm.dialer();
    let Dialer { pool, .. } = &dialer;
    let pool = Arc::clone(pool);
    let (telemetry, _seen) = recording_telemetry();
    let out = run_live_items(
        &swarm.store(),
        dialer,
        swarm.items(),
        live_run_opts(Duration::from_millis(1500), false, 32, telemetry),
        Duration::from_secs(60),
    )
    .await;
    out.assert_all_done(ITEMS);
    assert_eq!(pool.dials(), 1, "one provider, one dial, for twenty items");
    swarm.shutdown().await;
}

/// Items spread over BOTH providers: the least-loaded pick puts payload on each, by the providers' own socket
/// counters.
#[tokio::test]
async fn live_items_spread_over_both_providers() {
    const ITEMS: usize = 14;
    let swarm = live_swarm(2, ITEMS, 96 * 1024).await;
    let (a, b) = (&swarm.nodes[0], &swarm.nodes[1]);
    let (a_before, b_before) = (sent_bytes(a), sent_bytes(b));
    let (telemetry, seen) = recording_telemetry();
    let out = run_live_items(
        &swarm.store(),
        swarm.dialer(),
        swarm.items(),
        live_run_opts(Duration::from_secs(5), true, 32, telemetry),
        Duration::from_secs(60),
    )
    .await;
    out.assert_all_done(ITEMS);
    swarm.assert_landed().await;
    let a_sent = sent_bytes(a).saturating_sub(a_before);
    let b_sent = sent_bytes(b).saturating_sub(b_before);
    assert!(
        a_sent > SERVED_PAYLOAD_FLOOR && b_sent > SERVED_PAYLOAD_FLOOR,
        "the items must spread over BOTH providers — a sent {a_sent} B, b sent {b_sent} B \
         (floor {SERVED_PAYLOAD_FLOOR} B)"
    );
    assert!(
        !tried_providers(&seen).is_empty(),
        "the provider telemetry sink must receive at least one event"
    );
    swarm.shutdown().await;
}

/// A provider killed mid-transfer costs its in-flight items a provider switch,
/// not the run: the survivor finishes everything.
#[tokio::test]
async fn live_fetch_survives_a_provider_dying_mid_transfer() {
    const ITEMS: usize = 16;
    let swarm = live_swarm(2, ITEMS, 512 * 1024).await;
    let (a, b) = (&swarm.nodes[0], &swarm.nodes[1]);
    // 8 MiB paced at 2 MB/s PER PROVIDER puts the transfer in the multi-second
    // range, so the 150 ms kill lands mid-run on any machine; the assertion
    // right before the kill makes "mid-transfer" a fact, not a hope.
    a.set_upload_limit(2_000_000);
    b.set_upload_limit(2_000_000);
    let (a_before, b_before) = (sent_bytes(a), sent_bytes(b));
    let (telemetry, seen) = recording_telemetry();
    let finished = Arc::new(AtomicBool::new(false));
    let dialer = swarm.dialer();
    let run = async {
        let out = run_live_items(
            &swarm.store(),
            dialer,
            swarm.items(),
            live_run_opts(Duration::from_secs(5), true, 32, telemetry),
            Duration::from_secs(120),
        )
        .await;
        finished.store(true, Ordering::SeqCst);
        out
    };
    let kill = async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !finished.load(Ordering::SeqCst),
            "the run finished before the provider could be killed — the kill would \
             prove nothing; lower the upload limit or grow the items"
        );
        // Read A's contribution BEFORE killing it: this is the number that says
        // the victim was genuinely serving when it died.
        let a_sent = sent_bytes(a).saturating_sub(a_before);
        a.shutdown().await;
        a_sent
    };
    let (out, a_sent) = tokio::join!(run, kill);
    out.assert_all_done(ITEMS);
    swarm.assert_landed().await;
    let b_sent = sent_bytes(b).saturating_sub(b_before);
    assert!(
        a_sent > SERVED_PAYLOAD_FLOOR,
        "the provider we killed must have been serving payload when it died — \
         it sent only {a_sent} B (floor {SERVED_PAYLOAD_FLOOR} B)"
    );
    assert!(
        b_sent > SERVED_PAYLOAD_FLOOR,
        "the surviving provider must have served the rest — it sent only {b_sent} B \
         (floor {SERVED_PAYLOAD_FLOOR} B)"
    );
    assert!(
        !tried_providers(&seen).is_empty(),
        "the provider telemetry sink must receive at least one event"
    );
    swarm.nodes[1].shutdown().await;
    swarm.recv.shutdown().await;
}

/// A provider that ACCEPTS and then trickles must not pin the run: the stall
/// ceiling reassigns its items and the run finishes on the healthy provider
/// B is paced at 1 KB/s,
/// so a 16 KiB chunk needs 16 s and "no growth for 1500 ms" is unambiguous.
#[tokio::test]
async fn live_trickling_provider_is_reassigned() {
    const ITEMS: usize = 8;
    const SIZE: usize = 256 * 1024;
    let swarm = live_swarm(2, ITEMS, SIZE).await;
    swarm.nodes[1].set_upload_limit(1_000);
    let (telemetry, seen) = recording_telemetry();
    let started = Instant::now();
    let out = run_live_items(
        &swarm.store(),
        swarm.dialer(),
        swarm.items(),
        live_run_opts(Duration::from_millis(1500), true, 32, telemetry),
        Duration::from_secs(120),
    )
    .await;
    let elapsed = started.elapsed();
    out.assert_all_done(ITEMS);
    swarm.assert_landed().await;
    let a_stats = live_stats(&out.report, swarm.ids[0]);
    let b_stats = live_stats(&out.report, swarm.ids[1]);
    // B cannot have completed a single item: one 256 KiB item needs 262 s at
    // 1 KB/s and the ceiling cuts it at 1.5 s, so the bulk is A's.
    assert!(
        a_stats.bytes > 6 * SIZE as u64,
        "the healthy provider must have served the bulk — a delivered {} B of {} B, \
         b delivered {} B (elapsed {elapsed:?})",
        a_stats.bytes,
        (ITEMS * SIZE) as u64,
        b_stats.bytes
    );
    assert!(
        b_stats.failures >= 1,
        "the trickling provider must have been failed out by the stall ceiling, \
         not waited for — its stats are {b_stats:?}"
    );
    assert!(
        out.report.stalls >= 1,
        "the failure must be attributed to the progress deadline (a stall): {:?}",
        out.report
    );
    assert!(
        tried_providers(&seen).contains(swarm.ids[1].as_bytes()),
        "the engine must report the attempts it made on the trickling provider"
    );
    swarm.shutdown().await;
}

/// Every provider dead ⇒ the item exhausts its backoff ladder and fails inside a
/// bounded time instead of spinning. A live item keeps its verified bytes in
/// the store untagged.
#[tokio::test]
async fn live_item_fails_after_the_ladder_when_every_provider_is_dead() {
    let swarm = live_swarm(1, 0, 0).await;
    let dialer = swarm.dialer();
    // The address book keeps the corpse's address; nobody answers there.
    swarm.nodes[0].shutdown().await;
    let nobody_serves = Hash::new(b"a blob nobody serves");
    let (telemetry, _seen) = recording_telemetry();
    let started = Instant::now();
    let out = run_live_items(
        &swarm.store(),
        dialer,
        vec![live_blob_item("dead", nobody_serves, 64 * 1024, &swarm.ids)],
        // The ladder, not the stall watchdog, is what bounds this test.
        live_run_opts(super::assign::STALL_HARD_LIMIT, true, 8, telemetry),
        Duration::from_secs(120),
    )
    .await;
    let elapsed = started.elapsed();
    assert!(
        matches!(out.outcome("dead"), ItemOutcome::Failed(_)),
        "an item whose every provider is unreachable must fail: {:?}",
        out.outcomes
    );
    assert!(
        elapsed >= Duration::from_secs(5),
        "the engine must walk its backoff ladder before giving up, not fail on the \
         first dial — it gave up after {elapsed:?}"
    );
    assert!(
        out.verdicts
            .iter()
            .any(|v| matches!(v, LiveVerdict::DialFailed { .. })),
        "the dial failures are reported: {:?}",
        out.verdicts
    );
    swarm.recv.shutdown().await;
}

/// The report accounts every byte the providers' socket counters saw, and every
/// item is attributed to exactly one provider. Hedging is OFF here: once a hedge
/// can fire, a provider's `bytes` and its egress stop being the same quantity.
#[tokio::test]
async fn live_report_matches_provider_send_counters() {
    const ITEMS: usize = 14;
    let swarm = live_swarm(2, ITEMS, 256 * 1024).await;
    let (a, b) = (&swarm.nodes[0], &swarm.nodes[1]);
    let (a_before, b_before) = (sent_bytes(a), sent_bytes(b));
    let (telemetry, _) = recording_telemetry();
    let out = run_live_items(
        &swarm.store(),
        swarm.dialer(),
        swarm.items(),
        live_run_opts(Duration::from_secs(5), false, 32, telemetry),
        Duration::from_secs(120),
    )
    .await;
    let a_sent = sent_bytes(a).saturating_sub(a_before);
    let b_sent = sent_bytes(b).saturating_sub(b_before);
    out.assert_all_done(ITEMS);
    swarm.assert_landed().await;
    for (name, id, sent) in [("a", swarm.ids[0], a_sent), ("b", swarm.ids[1], b_sent)] {
        let stats = live_stats(&out.report, id);
        assert!(
            stats.bytes <= sent,
            "provider {name} cannot have delivered more payload than its socket sent: \
             report {} B vs sent {sent} B",
            stats.bytes
        );
        assert!(
            stats.bytes * 10 >= sent * 9,
            "provider {name}'s reported payload must account for its egress bar framing: \
             report {} B vs sent {sent} B ({:.1} %)",
            stats.bytes,
            stats.bytes as f64 * 100.0 / sent.max(1) as f64
        );
    }
    assert_eq!(
        out.report.total_children() as usize,
        ITEMS,
        "every item must be accounted for exactly once: {:?}",
        out.report
    );
    swarm.shutdown().await;
}

/// The hedge swarm: forty 256 KiB items on three providers. A live run's hedge
/// cap is never smaller than one hedge of the largest item in flight, so an
/// idle budget pays for the hedge of a stranded item however few items are left.
const LIVE_HEDGE_ITEMS: usize = 40;
const LIVE_HEDGE_SIZE: usize = 256 * 1024;

/// A provider that goes slow MID-TRANSFER triggers a hedge well before the stall
/// ceiling, and the run finishes on the healthy peers. Every provider is paced
/// the same, then one collapses to 1 KB/s: fast history, slow now. This pins the
/// trigger and the cap floor (an idle budget pays for one hedge), not a byte
/// bound: the hedge goes to a healthy peer and the collapsed primary moves ~0
/// bytes past the split, so nothing is duplicated.
#[tokio::test]
async fn live_hedge_fires_before_the_stall_ceiling_on_a_slow_provider() {
    live_hedge_fires_scenario(LIVE_HEDGE_ITEMS, LIVE_HEDGE_ITEMS).await;
}

/// The same scenario with the production receive-stream limit (8) and eight
/// items, all in flight from the start. When the trigger fires only the two or
/// three stranded items are still in flight, and 5 % of those cannot pay for a
/// hedge of one of them; the half-item floor does.
#[tokio::test]
async fn live_hedge_fires_with_the_default_stream_limit() {
    const ITEMS: usize = 8;
    let (report, _) = live_hedge_fires_scenario(ITEMS, 8).await;
    // Not a byte bound (the budget bounds bytes): a sanity bound against a
    // re-arm loop on the same item.
    assert!(
        report.hedges as usize <= 2 * ITEMS,
        "a hedge storm: {report:?}"
    );
    // Every byte the providers moved is payload or counted as duplicated: a
    // hedge armed beside a primary that is still moving, near the end of an
    // item, has both sides fetch the last group or two.
    let payload = (ITEMS * LIVE_HEDGE_SIZE) as u64;
    assert_eq!(
        report.total_bytes() - report.hedge_bytes,
        payload,
        "bytes fetched twice that no hedge was charged for: {report:?}"
    );
}

/// More items than streams: forty at the production limit of 8. The uplink
/// meter reads the collapsed provider slow within a window, so its stranded
/// items are hedged away and NO new item is handed to it. A new item there
/// would have no size for ~16 s (one 16 KiB leaf at 1 KB/s), could not be
/// hedged, and would sit until the ceiling: with the least-loaded pick this
/// run took 100 to 110 s with 1 to 3 stalls.
#[tokio::test]
async fn live_new_items_do_not_land_on_a_provider_that_went_slow() {
    let (report, tries_on_collapsed) = live_hedge_fires_scenario(LIVE_HEDGE_ITEMS, 8).await;
    // Its share of the first round is 2 or 3 of the 8 streams, handed out
    // before it collapsed. Nothing completes inside the meter's first window
    // (an item needs ~2 s), so no pick falls in the gap before it is read.
    assert!(
        (1..=3).contains(&tries_on_collapsed),
        "the collapsed provider must get its first-round items and no more — it was \
         handed {tries_on_collapsed} of {LIVE_HEDGE_ITEMS} ({report:?})"
    );
    assert!(
        report.hedges as usize <= 2 * LIVE_HEDGE_ITEMS,
        "a hedge storm: {report:?}"
    );
}

/// A provider that is slow from the start (20 KB/s beside two at 400) is tried,
/// read, and left alone: it gets the items the first round spreads over an
/// unread swarm and no more. Its first window on the uplink meter reads it
/// slow, so when a hedge rescues one of its items the slot goes to a fast
/// provider. With a least-loaded pick every rescue freed a slot there for a
/// new item to strand: it was handed 6 of the 40.
#[tokio::test]
async fn live_a_slow_provider_is_read_once_and_left_alone() {
    const ITEMS: usize = 40;
    let swarm = live_swarm(3, ITEMS, LIVE_HEDGE_SIZE).await;
    for n in &swarm.nodes {
        n.set_upload_limit(400_000);
    }
    swarm.nodes[2].set_upload_limit(20_000);
    let slow = *swarm.ids[2].as_bytes();
    let (telemetry, seen) = recording_telemetry();
    let out = run_live_items(
        &swarm.store(),
        swarm.dialer(),
        swarm.items(),
        live_run_opts(super::assign::STALL_HARD_LIMIT, true, 8, telemetry),
        Duration::from_secs(120),
    )
    .await;
    out.assert_all_done(ITEMS);
    swarm.assert_landed().await;
    let tries_on_slow = seen
        .lock()
        .expect("telemetry mutex poisoned")
        .iter()
        .filter(|e| matches!(e, ProviderEvent::Trying(id) if *id == slow))
        .count();
    // The first round hands it 2 or 3 of the 8 streams; one more is slack for
    // an item taken before the first reading lands.
    assert!(
        (1..=4).contains(&tries_on_slow),
        "the slow provider must be tried, then left alone — it was handed {tries_on_slow} \
         of {ITEMS} items ({:?})",
        out.report
    );
    swarm.shutdown().await;
}

/// Returns the run's report and how many times the collapsing provider was
/// tried (as a primary or as a hedge's provider).
async fn live_hedge_fires_scenario(
    items: usize,
    max_in_flight: usize,
) -> (AssignmentReport, usize) {
    let swarm = live_swarm(3, items, LIVE_HEDGE_SIZE).await;
    for n in &swarm.nodes {
        n.set_upload_limit(400_000);
    }
    let collapsing = *swarm.ids[2].as_bytes();
    let (telemetry, seen) = recording_telemetry();
    let collapse = {
        let slow = Arc::clone(&swarm.nodes[2]);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            slow.set_upload_limit(1_000);
        })
    };
    let started = Instant::now();
    let out = run_live_items(
        &swarm.store(),
        swarm.dialer(),
        swarm.items(),
        // The 60 s ceiling is long enough that it cannot be what rescues the run.
        live_run_opts(Duration::from_secs(60), true, max_in_flight, telemetry),
        Duration::from_secs(120),
    )
    .await;
    let elapsed = started.elapsed();
    collapse.await.ok();
    out.assert_all_done(items);
    swarm.assert_landed().await;
    assert!(
        elapsed < Duration::from_secs(45),
        "hedging must finish before the 60 s ceiling could — took {elapsed:?} ({:?})",
        out.report
    );
    assert!(
        out.report.hedges >= 1,
        "at least one assignment must have been hedged: {:?}",
        out.report
    );
    assert_eq!(
        out.report.stalls, 0,
        "the ceiling must not be what rescued it — hedging must have got there first: {:?}",
        out.report
    );
    swarm.shutdown().await;
    let tries = seen
        .lock()
        .expect("telemetry mutex poisoned")
        .iter()
        .filter(|e| matches!(e, ProviderEvent::Trying(id) if *id == collapsing))
        .count();
    (out.report, tries)
}

/// The loser is REALLY cancelled: the slow provider's own socket send counter
/// stays under four items' worth though it was handed many more. The byte bound
/// on `hedge_bytes` is only a gross sanity check (the loser is the slow peer
/// and moves ~0 bytes past the split). At 5 KB/s a 256 KiB item needs ~52 s, so
/// the peer never completes one and never measures itself into hedge immunity.
#[tokio::test]
async fn live_hedge_cancels_the_loser_and_bounds_duplicate_bytes() {
    live_hedge_cancels_scenario().await;
}

async fn live_hedge_cancels_scenario() {
    let swarm = live_swarm(3, LIVE_HEDGE_ITEMS, LIVE_HEDGE_SIZE).await;
    let slow = &swarm.nodes[2];
    slow.set_upload_limit(5_000);
    let (telemetry, _) = recording_telemetry();
    let slow_before = sent_bytes(slow);
    let started = Instant::now();
    let out = run_live_items(
        &swarm.store(),
        swarm.dialer(),
        swarm.items(),
        live_run_opts(
            Duration::from_secs(60),
            true,
            LIVE_HEDGE_ITEMS,
            telemetry,
        ),
        // A byte budget, not a clock: a loaded runner must not fail it. An
        // uncancelled primary would need about eleven minutes.
        Duration::from_secs(240),
    )
    .await;
    let elapsed = started.elapsed();
    let slow_sent = sent_bytes(slow).saturating_sub(slow_before);
    out.assert_all_done(LIVE_HEDGE_ITEMS);
    swarm.assert_landed().await;
    assert!(
        out.report.hedges >= 1,
        "the test needs a hedge to have fired: {:?}",
        out.report
    );
    // A hedge's back half can be the last of its item; it must still be credited.
    assert_eq!(
        out.report.total_children() as usize,
        LIVE_HEDGE_ITEMS,
        "every item must be credited exactly once, hedge-won ones included: {:?}",
        out.report
    );
    // The slow peer's primaries were each cancelled by a hedge that finished the
    // item. What it moved before that is payload the run kept, so it is credited;
    // being cancelled is not a failure, so no strike is.
    let slow_stats = live_stats(&out.report, swarm.ids[2]);
    assert!(
        slow_stats.bytes > 0 && !slow_stats.elapsed.is_zero(),
        "a cancelled hedge loser must be credited what it moved: {:?}",
        out.report
    );
    assert_eq!(
        slow_stats.failures, 0,
        "a cancelled loser is not a failure: {:?}",
        out.report
    );
    const CANCEL_BOUND: u64 = 4 * (LIVE_HEDGE_SIZE as u64) + SERVED_PAYLOAD_FLOOR;
    assert!(
        slow_sent < CANCEL_BOUND,
        "a cancelled primary must stop sending — the slow provider put {slow_sent} B on the \
         wire in {elapsed:?}, over the {CANCEL_BOUND} B a peer cancelled out of every \
         assignment it was given can account for"
    );
    let total = swarm.total();
    assert!(
        out.report.hedge_bytes <= (total as f64 * 0.6) as u64,
        "hedge deliveries must stay a minority of the payload: {} B of {total} B",
        out.report.hedge_bytes
    );
    swarm.shutdown().await;
}

/// Many items stranded on a peer at once: hedging engages, every item lands
/// and is credited once, and `hedge_bytes` stays under twice the run's 5 %.
/// This is a gross end-to-end bound, not a pin of the budget's rule — the
/// hedges go to healthy peers and the collapsed primary moves ~0 bytes past the
/// split, so little is duplicated whatever the budget; the rule itself is
/// pinned by the `HedgeBudget` unit tests in `assign.rs`.
#[tokio::test]
async fn live_hedge_budget_stops_a_storm() {
    let swarm = live_swarm(3, LIVE_HEDGE_ITEMS, LIVE_HEDGE_SIZE).await;
    for n in &swarm.nodes {
        n.set_upload_limit(400_000);
    }
    let (telemetry, _) = recording_telemetry();
    // Every item the slow peer holds is stranded at the same instant.
    let collapse = {
        let slow = Arc::clone(&swarm.nodes[2]);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            slow.set_upload_limit(1_000);
        })
    };
    let out = run_live_items(
        &swarm.store(),
        swarm.dialer(),
        swarm.items(),
        live_run_opts(
            Duration::from_secs(60),
            true,
            LIVE_HEDGE_ITEMS,
            telemetry,
        ),
        Duration::from_secs(120),
    )
    .await;
    collapse.await.ok();
    out.assert_all_done(LIVE_HEDGE_ITEMS);
    swarm.assert_landed().await;
    assert!(
        out.report.hedges >= 1,
        "the test is only meaningful if hedging engaged at all: {:?}",
        out.report
    );
    let cap = (swarm.total() as f64 * 0.05) as u64;
    assert_eq!(
        out.report.total_children() as usize,
        LIVE_HEDGE_ITEMS,
        "every item must be credited exactly once, hedge-won ones included: {:?}",
        out.report
    );
    assert!(
        out.report.hedge_bytes <= 2 * cap,
        "hedging must stay inside its byte budget — it duplicated {} B against a {cap} B cap \
         (2x allowed) over {} hedges: {:?}",
        out.report.hedge_bytes,
        out.report.hedges,
        out.report
    );
    swarm.shutdown().await;
}

/// The stall ceiling and the hedge budget are independent rules: whatever the
/// budget decides, the progress deadline fires on its own schedule and reassigns
/// the item. The slow
/// peer dribbles at 1 KB/s from the start, so a 1500 ms deadline is unambiguous.
#[tokio::test]
async fn live_stall_ceiling_is_independent_of_the_hedge_budget() {
    const ITEMS: usize = 6;
    let swarm = live_swarm(3, ITEMS, 512 * 1024).await;
    swarm.nodes[2].set_upload_limit(1_000);
    let (telemetry, _) = recording_telemetry();
    let out = run_live_items(
        &swarm.store(),
        swarm.dialer(),
        swarm.items(),
        // Hedging is ON throughout; the hedge count is deliberately not asserted.
        live_run_opts(Duration::from_millis(1500), true, ITEMS, telemetry),
        Duration::from_secs(90),
    )
    .await;
    out.assert_all_done(ITEMS);
    swarm.assert_landed().await;
    let slow = live_stats(&out.report, swarm.ids[2]);
    assert!(
        slow.failures >= 1,
        "the 1500 ms progress deadline must fail the trickling provider out while hedging \
         is enabled: {slow:?}"
    );
    assert!(
        out.report.stalls >= 1,
        "and that failure must be attributed to the deadline: {:?}",
        out.report
    );
    swarm.shutdown().await;
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

    /// Task 10: `LiveRunOptions.delivered` is forwarded into every child
    /// [`crate::sharing::iroh::assign::AssignmentOptions`] `run_live` builds,
    /// and `transfer_once` feeds it the exact bytes each provider delivered —
    /// their sum equals what the report records for the run.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delivered_deltas_sum_to_the_reported_payload_bytes() {
        let rig = ts::landed_rig(2).await;
        let me = ts::bare_node().await;
        ts::pair(&me, &rig.node).await;
        let store = ts::scratch_store();
        let provider = rig.node.endpoint_addr().id;
        let seen: Arc<std::sync::Mutex<Vec<(String, [u8; 32], u64)>>> = Default::default();
        let sink: crate::sharing::iroh::assign::DeliveredSink = {
            let seen = Arc::clone(&seen);
            Arc::new(move |k: &str, p: [u8; 32], d: u64| {
                seen.lock().unwrap().push((k.to_string(), p, d))
            })
        };
        let mut opts = ts::live_opts(2, crate::sharing::noop_provider_telemetry());
        opts.delivered = Some(sink);
        let (item0, _c0) = ts::rig_item(&rig, 0, ProviderSet::Fixed(Arc::new(vec![provider])));
        let (item1, _c1) = ts::rig_item(&rig, 1, ProviderSet::Fixed(Arc::new(vec![provider])));
        let run = ts::drive_live(
            &store,
            ts::live_dialer(&me, &[&rig.node]),
            vec![item0, item1],
            opts,
            async {},
        )
        .await;
        let seen = seen.lock().unwrap();
        let total: u64 = seen.iter().map(|(_, _, d)| d).sum();
        assert_eq!(total, run.report.total_bytes());
        assert!(seen.iter().all(|(_, p, _)| *p == *provider.as_bytes()));
        let keys: std::collections::BTreeSet<&str> =
            seen.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(keys.len(), 2, "both items reported under their own keys");
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
            Dialer {
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
