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

    provider.serve(&announce, &pkg_dir, None).await.unwrap();
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
    provider.serve(&announce, &dir, None).await.unwrap();

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
    t1.serve(&announce, &dir, None).await.unwrap();
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
    receiver.serve(&announce, &dir, None).await.unwrap();
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

    // A multi-megabyte package makes it likely the first fetch is interrupted
    // mid-download; if it happens to finish, the re-fetch below is idempotent.
    let (pkg_dir, announce) = build_package(
        &tmp.path().join("src"),
        "uuid-r",
        "big.fits",
        "M31",
        16 * 1024 * 1024,
    );
    provider.serve(&announce, &pkg_dir, None).await.unwrap();
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
    // First attempt: cancel quickly (drops the download future mid-flight).
    let _ = tokio::time::timeout(
        Duration::from_millis(80),
        receiver.fetch(provider_info.node_id, &wire, &dest, noop_fetch_sink()),
    )
    .await;

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
    sender.serve(&announce, &pkg_dir, None).await.unwrap();

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
    provider.serve(&announce, &pkg_dir, None).await.unwrap();
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
        .serve(&announce, &pkg_dir, Some(&want))
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
        .serve(&announce, &pkg_dir, Some(&empty))
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
    provider.serve(&announce, &pkg_dir, None).await.unwrap();

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
    provider.serve(&announce, &pkg_dir, None).await.unwrap();

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
    provider.serve(&announce, &pkg_dir, None).await.unwrap();
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
    provider.serve(&announce, &pkg_dir, None).await.unwrap();
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

    provider.serve(&announce, &pkg_dir, None).await.unwrap();
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

    provider.serve(&announce, &pkg_dir, None).await.unwrap();
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
    a_out.serve(&announce, &pkg_dir, None).await.unwrap();
    b_out.serve(&announce, &pkg_dir, None).await.unwrap();

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
    a_out.serve(&announce, &pkg_dir, None).await.unwrap();
    b_out.serve(&announce, &pkg_dir, None).await.unwrap();
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
