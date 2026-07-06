//! Acceptance floor for task A4: the five named state-machine tests.
//!
//! Each drives the [`SyncEngine`] end to end over the in-process
//! [`LoopbackTransport`](crate::sharing::loopback::LoopbackTransport) + a
//! [`StandaloneSyncStore`] in a tempdir. The engine is the *sender*; the test
//! plays a reactive *receiver* that fetches on every `AnnounceReceived` and acks
//! on success — so a re-announce (fresh enqueue, retry, or crash-resume)
//! automatically re-triggers a transfer without any per-test wiring.
//!
//! "Kill/restart" = drop the engine handle mid-flight and construct a new engine
//! (fresh transport endpoint) over the same store file; it re-enumerates
//! `non_terminal()` and finishes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;
use tokio::time::Instant;

use crate::package::{self, write_package, ManifestRecord, PayloadKind, MANIFEST_VERSION};
use crate::sharing::loopback::{FaultPlan, LoopbackNetwork, LoopbackTransport};
use crate::sharing::types::{FrameReceipt, ReceiptOutcome, TransportEvent};
use crate::sharing::SharingTransport;

use super::store::{StandaloneSyncStore, SyncStore};
use super::{HistoryQuery, OutboundState, SyncConfig, SyncEngine};

/// Build a one-frame package under `src_root`'s parent and return its directory.
///
/// Writes a `size`-byte payload named `filename`, then a package whose manifest
/// carries `frame_uuid` and an `object` in `frame_meta`. Returns the package dir
/// the engine will serve.
fn build_package(
    src_root: &Path,
    frame_uuid: &str,
    filename: &str,
    object: &str,
    size: usize,
) -> PathBuf {
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
        frame_meta: serde_json::json!({ "filename": filename, "object": object }),
        analysis: None,
        app_version: "test".to_string(),
    };

    let pkg_dir = src_root.parent().unwrap().join(format!("pkg-{frame_uuid}"));
    write_package(&pkg_dir, vec![(payload, record)]).unwrap();
    pkg_dir
}

/// Counters a spawned receiver bumps so tests can observe its behaviour.
struct ReceiverStats {
    attempts: Arc<AtomicUsize>,
    failures: Arc<AtomicUsize>,
}

/// Spawn a reactive receiver on `endpoint`: for every `AnnounceReceived`, fetch
/// into a fresh dir and (on success) ack every manifest frame as `Ingested`.
/// Fetch faults (e.g. an injected abort) count a failure and skip the ack.
fn spawn_receiver(endpoint: Arc<LoopbackTransport>, dest_root: PathBuf) -> ReceiverStats {
    let attempts = Arc::new(AtomicUsize::new(0));
    let failures = Arc::new(AtomicUsize::new(0));
    let (a, f) = (attempts.clone(), failures.clone());

    tokio::spawn(async move {
        let mut events = endpoint.events().await;
        let mut n = 0usize;
        while let Some(event) = events.recv().await {
            let TransportEvent::AnnounceReceived { from, announce } = event else {
                continue;
            };
            n += 1;
            a.fetch_add(1, SeqCst);
            let dest = dest_root.join(format!("fetch-{n}"));
            match endpoint.fetch(from, &announce, &dest).await {
                Ok(()) => {
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
                    let _ = endpoint.ack(from, &announce.package_id, receipts).await;
                }
                Err(_) => {
                    f.fetch_add(1, SeqCst);
                }
            }
        }
    });

    ReceiverStats { attempts, failures }
}

/// Poll `pred` every 10ms until true, panicking after `timeout`.
async fn wait_until<F: FnMut() -> bool>(mut pred: F, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("wait_until timed out after {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

const WAIT: Duration = Duration::from_secs(5);

fn state_of(store: &StandaloneSyncStore, id: i64) -> Option<OutboundState> {
    store.get_outbound(id).unwrap().map(|r| r.state)
}

// ---------------------------------------------------------------------------
// The five named acceptance tests.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn happy_path_reaches_confirmed_and_history_has_both_events() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let pkg = build_package(&tmp.path().join("src1"), "uuid-1", "frame1.fits", "M42", 4096);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();
    wait_until(
        || state_of(&store, id) == Some(OutboundState::Confirmed),
        WAIT,
    )
    .await;

    let row = store.get_outbound(id).unwrap().unwrap();
    assert_eq!(row.state, OutboundState::Confirmed);
    assert!(row.confirmed_at.is_some(), "confirmed_at must be stamped");

    let history = store
        .search_history(HistoryQuery {
            filename: Some("frame1.fits".to_string()),
            object: None,
            limit: 100,
        })
        .unwrap();
    assert_eq!(
        history.len(),
        2,
        "history must record both the transfer-start and the confirm event"
    );
    assert!(
        history
            .iter()
            .any(|h| h.finished_at.is_none() && h.outcome == "sent"),
        "expected a 'sent' start event"
    );
    assert!(
        history
            .iter()
            .any(|h| h.finished_at.is_some() && h.outcome == "ingested"),
        "expected an 'ingested' confirm event"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn mid_transfer_abort_leaves_transferring_then_resume_completes() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    // Abort the receiver's first fetch partway through the payload.
    receiver.set_fault(FaultPlan {
        abort_after_bytes: Some(64),
        ..Default::default()
    });
    let stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let pkg = build_package(&tmp.path().join("src2"), "uuid-2", "frame2.fits", "M31", 4096);
    let db_path = tmp.path().join("sync.db");

    // Engine A: long ack timeout so it will not retry before we kill it.
    let store_a = Arc::new(StandaloneSyncStore::open(&db_path).unwrap());
    let engine_a = SyncEngine::spawn_with_config(
        store_a.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig {
            ack_timeout: Duration::from_secs(60),
            max_attempts: 5,
        },
    );
    let id = engine_a.enqueue_package(&pkg).await.unwrap();

    // The receiver aborts its first fetch; the row rests in Transferring.
    wait_until(|| stats.failures.load(SeqCst) >= 1, WAIT).await;
    wait_until(
        || state_of(&store_a, id) == Some(OutboundState::Transferring),
        WAIT,
    )
    .await;
    assert_eq!(
        state_of(&store_a, id),
        Some(OutboundState::Transferring),
        "aborted transfer must leave the row Transferring"
    );

    // Kill the engine mid-flight (drop the handle → worker stops).
    drop(engine_a);

    // Restart over the SAME store file with a fresh transport endpoint.
    let store_b = Arc::new(StandaloneSyncStore::open(&db_path).unwrap());
    let engine_b = SyncEngine::spawn(
        store_b.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
    );

    wait_until(
        || state_of(&store_b, id) == Some(OutboundState::Confirmed),
        WAIT,
    )
    .await;
    assert_eq!(
        state_of(&store_b, id),
        Some(OutboundState::Confirmed),
        "resume must re-announce and complete"
    );
    assert!(
        stats.attempts.load(SeqCst) >= 2,
        "resume should have triggered a second fetch"
    );

    engine_b.shutdown().await;
}

#[tokio::test]
async fn ack_lost_then_duplicate_ack_confirms_once() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    // The transport delivers the ack twice (at-least-once): the engine must
    // confirm exactly once and never double-write history.
    receiver.set_fault(FaultPlan {
        duplicate_ack: true,
        ..Default::default()
    });
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let pkg = build_package(&tmp.path().join("src3"), "uuid-3", "frame3.fits", "M13", 2048);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();
    wait_until(
        || state_of(&store, id) == Some(OutboundState::Confirmed),
        WAIT,
    )
    .await;
    // Give any duplicate ack time to arrive and be (correctly) ignored.
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(state_of(&store, id), Some(OutboundState::Confirmed));

    let history = store
        .search_history(HistoryQuery {
            filename: Some("frame3.fits".to_string()),
            object: None,
            limit: 100,
        })
        .unwrap();
    let confirmed: Vec<_> = history
        .iter()
        .filter(|h| h.finished_at.is_some())
        .collect();
    assert_eq!(
        confirmed.len(),
        1,
        "a duplicate ack must not produce a second confirm history row"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn failed_after_max_attempts_with_error_outcome_in_history() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Receiver endpoint is started (so announce can be delivered) but has NO
    // reactive loop — it never acks, so every attempt times out.
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;

    let pkg = build_package(&tmp.path().join("src4"), "uuid-4", "frame4.fits", "NGC7000", 1024);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig {
            ack_timeout: Duration::from_millis(40),
            max_attempts: 5,
        },
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Failed), WAIT).await;

    let row = store.get_outbound(id).unwrap().unwrap();
    assert_eq!(row.state, OutboundState::Failed);
    assert_eq!(row.attempts, 5, "should fail after exactly max_attempts");

    let history = store
        .search_history(HistoryQuery {
            filename: Some("frame4.fits".to_string()),
            object: None,
            limit: 100,
        })
        .unwrap();
    assert!(
        history.iter().any(|h| h.outcome == "failed"),
        "a failed outcome must be recorded in history"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn cancel_moves_to_failed_cancelled() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Receiver started but not acking, so the package stays in flight until we
    // cancel it. Long ack timeout so a timeout does not race the cancel.
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;

    let pkg = build_package(&tmp.path().join("src5"), "uuid-5", "frame5.fits", "Sol", 1024);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig {
            ack_timeout: Duration::from_secs(60),
            max_attempts: 5,
        },
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();
    wait_until(
        || state_of(&store, id) == Some(OutboundState::Transferring),
        WAIT,
    )
    .await;

    engine.cancel(id).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Failed), WAIT).await;

    assert_eq!(state_of(&store, id), Some(OutboundState::Failed));
    let history = store
        .search_history(HistoryQuery {
            filename: Some("frame5.fits".to_string()),
            object: None,
            limit: 100,
        })
        .unwrap();
    assert!(
        history.iter().any(|h| h.outcome == "cancelled"),
        "a cancelled outcome must be recorded in history"
    );

    engine.shutdown().await;
}
