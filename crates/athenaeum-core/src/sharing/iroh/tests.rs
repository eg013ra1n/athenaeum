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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use iroh::{EndpointId, RelayMode};
use iroh_blobs::format::collection::Collection;
use tempfile::tempdir;
use tokio::sync::mpsc::Receiver;
use tokio::time::Instant;

use crate::package::{self, write_package, ManifestRecord, PayloadKind, MANIFEST_VERSION};
use crate::sharing::types::{FrameReceipt, PackageAnnounce, ReceiptOutcome, TransportEvent};
use crate::sharing::SharingTransport;
use crate::sync::{HistoryQuery, OutboundState, StandaloneSyncStore, SyncEngine, SyncStore};

use super::{random_secret, BlobStore, IrohTransport};

/// Generous ceiling for a single event / transfer over a freshly-established
/// QUIC connection (bind + handshake + transfer), well above the localhost norm.
const IROH_WAIT: Duration = Duration::from_secs(60);

/// Build a fresh in-memory transport with the relay disabled (direct localhost).
async fn mem_transport() -> IrohTransport {
    IrohTransport::new(random_secret(), RelayMode::Disabled, BlobStore::Memory)
        .await
        .expect("build iroh transport")
}

/// Bring two endpoints online and pair them (each learns the other's address).
async fn start_and_pair(a: &IrohTransport, b: &IrohTransport) -> (crate::sharing::types::StartInfo, crate::sharing::types::StartInfo) {
    let a_info = a.start().await.expect("start a");
    let b_info = b.start().await.expect("start b");
    a.add_peer_ticket(&b_info.pairing_ticket).expect("a pairs b");
    b.add_peer_ticket(&a_info.pairing_ticket).expect("b pairs a");
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
    let (pkg_dir, announce) =
        build_package(&tmp.path().join("src"), "uuid-1", "frame1.fits", "M42", 128 * 1024);

    provider.serve(&announce, &pkg_dir).await.unwrap();
    provider
        .announce(receiver_info.node_id, &announce)
        .await
        .unwrap();

    // Receiver observes the announce — now carrying the iroh collection hash.
    let wire = match recv_next(&mut receiver_events).await {
        TransportEvent::AnnounceReceived { from, announce } => {
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

    // Receiver fetches into its own dir and verifies content + manifest.
    let dest = tempdir().unwrap();
    receiver
        .fetch(provider_info.node_id, &wire, dest.path())
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
    provider.serve(&announce, &dir).await.unwrap();

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
    provider.announce(ir.node_id, &announce).await.unwrap();
    let wire = match recv_next(&mut receiver_events).await {
        TransportEvent::AnnounceReceived { announce, .. } => announce,
        other => panic!("expected AnnounceReceived, got {other:?}"),
    };

    let dest = tempdir().unwrap();
    receiver
        .fetch(ip.node_id, &wire, dest.path())
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
    let t1 = IrohTransport::new(random_secret(), RelayMode::Disabled, BlobStore::Fs(home.path().to_path_buf()))
        .await
        .unwrap();
    t1.start().await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (dir, announce) = build_package(tmp.path(), "uuid-sweep-1", "s.fits", "M1", 2048);
    t1.serve(&announce, &dir).await.unwrap();
    t1.shutdown().await;

    // New process over the same store: the old tag must be gone after start().
    let t2 = IrohTransport::new(random_secret(), RelayMode::Disabled, BlobStore::Fs(home.path().to_path_buf()))
        .await
        .unwrap();
    t2.start().await.unwrap();
    let tag = package_tag(&announce.package_id);
    assert!(t2.store.tags().get(tag.as_bytes()).await.unwrap().is_none());
    t2.shutdown().await;
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
    let (pkg_dir, announce) =
        build_package(&tmp.path().join("src"), "uuid-r", "big.fits", "M31", 16 * 1024 * 1024);
    provider.serve(&announce, &pkg_dir).await.unwrap();
    provider
        .announce(receiver_info.node_id, &announce)
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
        receiver.fetch(provider_info.node_id, &wire, &dest),
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
    )
    .await
    .unwrap();
    receiver2.start().await.unwrap();
    receiver2
        .add_peer_ticket(&provider_info.pairing_ticket)
        .unwrap();

    receiver2
        .fetch(provider_info.node_id, &wire, &dest)
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
/// into a fresh dir and ack every manifest frame as `Ingested` (twice when
/// `duplicate_ack`). Mirrors the loopback engine tests' receiver.
fn spawn_iroh_receiver(receiver: Arc<IrohTransport>, dest_root: PathBuf, duplicate_ack: bool) {
    tokio::spawn(async move {
        let mut events = receiver.events().await;
        let mut n = 0usize;
        while let Some(ev) = events.recv().await {
            let TransportEvent::AnnounceReceived { from, announce } = ev else {
                continue;
            };
            n += 1;
            let dest = dest_root.join(format!("fetch-{n}"));
            if receiver.fetch(from, &announce, &dest).await.is_ok() {
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
                    let _ = receiver.ack(from, &announce.package_id, receipts.clone()).await;
                }
            }
        }
    });
}

#[tokio::test]
async fn engine_suite_over_iroh() {
    let tmp = tempdir().unwrap();
    let sender = Arc::new(mem_transport().await);
    let receiver = Arc::new(mem_transport().await);
    let (_sender_info, receiver_info) = start_and_pair(&sender, &receiver).await;
    let receiver_id = receiver_info.node_id;

    spawn_iroh_receiver(receiver.clone(), tmp.path().join("recv"), false);

    let (pkg_dir, _announce) =
        build_package(&tmp.path().join("src"), "uuid-e1", "frame_e1.fits", "M42", 256 * 1024);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        sender.clone() as Arc<dyn SharingTransport>,
        receiver_id,
    );

    let id = engine.enqueue_package(&pkg_dir).await.unwrap();
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
    spawn_iroh_receiver(receiver.clone(), tmp.path().join("recv"), true);

    let (pkg_dir, _announce) =
        build_package(&tmp.path().join("src"), "uuid-e2", "frame_e2.fits", "M13", 128 * 1024);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        sender.clone() as Arc<dyn SharingTransport>,
        receiver_id,
    );

    let id = engine.enqueue_package(&pkg_dir).await.unwrap();
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
    let (pkg_dir, announce) =
        build_package(&tmp.path().join("src"), "uuid-bare", "frame_bare.fits", "M1", 4096);
    sender.serve(&announce, &pkg_dir).await.unwrap();

    let err = sender
        .announce(receiver_info.node_id, &announce)
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
    assert!(!dest.exists(), "dest_dir must not be created on a rejected entry");
    assert!(
        !dest_parent.path().join("escape_relative.bin").exists(),
        "traversal entry must not write outside dest_dir"
    );
    assert!(
        !abs_target.exists(),
        "absolute-path entry must not write to an arbitrary path"
    );

    provider.shutdown().await;
    receiver.shutdown().await;
}
