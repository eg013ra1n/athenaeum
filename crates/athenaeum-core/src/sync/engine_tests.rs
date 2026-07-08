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

use crate::package::{
    self, write_package, ManifestRecord, PayloadKind, MANIFEST_FILENAME, MANIFEST_VERSION,
};
use crate::sharing::loopback::{FaultPlan, LoopbackNetwork, LoopbackTransport};
use crate::sharing::types::{FrameReceipt, PackageAnnounce, ReceiptOutcome, TransportEvent};
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

fn attempts_of(store: &StandaloneSyncStore, id: i64) -> u32 {
    store.get_outbound(id).unwrap().map(|r| r.attempts).unwrap_or(0)
}

/// Sorted file/dir names directly inside `dir` (for asserting a package dir's
/// contents after cleanup).
fn dir_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
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

/// Task 3: once a package reaches `Confirmed`, the sender must **release** its
/// served blobs — a fresh fetch of the same announce from the sender then fails
/// with "not served". Release is fire-and-forget (a detached task off the
/// synchronous confirm), so the assertion polls until it lands.
#[tokio::test]
async fn confirmed_package_is_released_from_transport() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Receiver that captures the announce it sees (so we can re-fetch it after
    // confirm) and acks every frame as ingested.
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let captured: Arc<std::sync::Mutex<Option<PackageAnnounce>>> =
        Arc::new(std::sync::Mutex::new(None));
    {
        let receiver = receiver.clone();
        let captured = captured.clone();
        let dest_root = tmp.path().join("recv");
        tokio::spawn(async move {
            let mut events = receiver.events().await;
            let mut n = 0usize;
            while let Some(event) = events.recv().await {
                let TransportEvent::AnnounceReceived { from, announce } = event else {
                    continue;
                };
                *captured.lock().unwrap() = Some(announce.clone());
                n += 1;
                let dest = dest_root.join(format!("fetch-{n}"));
                if receiver.fetch(from, &announce, &dest).await.is_ok() {
                    let Ok(records) = package::read_manifest(&dest) else {
                        continue;
                    };
                    let receipts: Vec<FrameReceipt> = records
                        .iter()
                        .map(|r| FrameReceipt {
                            frame_uuid: r.frame_uuid.clone(),
                            xxh3: r.xxh3.clone(),
                            outcome: ReceiptOutcome::Ingested,
                        })
                        .collect();
                    let _ = receiver.ack(from, &announce.package_id, receipts).await;
                }
            }
        });
    }

    let pkg = build_package(&tmp.path().join("src_rel"), "uuid-rel", "rel.fits", "M42", 4096);

    // Keep the sender endpoint so we know its node id (it is moved into the
    // engine as a trait object; the clone here shares the same registry entry).
    let sender_ep = Arc::new(net.endpoint());
    let sender_node = sender_ep.node_id();

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        sender_ep as Arc<dyn SharingTransport>,
        receiver_id,
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;

    let captured_announce = captured
        .lock()
        .unwrap()
        .clone()
        .expect("receiver must have seen the announce");

    // After confirm the sender must have released: a fresh fetch of the same
    // announce from the sender now fails "not served". Poll (release is spawned).
    let dest = tempdir().unwrap();
    let deadline = Instant::now() + WAIT;
    let err = loop {
        match receiver
            .fetch(sender_node, &captured_announce, dest.path())
            .await
        {
            Err(e) => break e,
            Ok(()) => {
                if Instant::now() >= deadline {
                    panic!("sender still serves the package after confirm (release did not fire)");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    };
    assert!(err.to_string().contains("not served"), "got: {err}");

    engine.shutdown().await;
}

/// A [`ProgressEmitter`](crate::events::ProgressEmitter) that records every
/// emitted `(event_name, payload)` for assertions (task M3 sender events).
struct CapturingEmitter(Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>);
impl crate::events::ProgressEmitter for CapturingEmitter {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        self.0.lock().unwrap().push((event_name.to_string(), payload));
    }
}

/// Task M3: the app-side engine (spawned WITH an emitter) surfaces coarse
/// send-side `sync-progress` + a single `sync-finished` per package — discrete
/// per state change, `direction = "sent"`, never per-byte spam.
#[tokio::test]
async fn sender_emits_coarse_progress_and_finished_events() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let pkg = build_package(&tmp.path().join("src_evt"), "uuid-evt", "evt.fits", "M42", 2048);
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());

    let events = Arc::new(std::sync::Mutex::new(Vec::<(String, serde_json::Value)>::new()));
    let emitter: Arc<dyn crate::events::ProgressEmitter> = Arc::new(CapturingEmitter(events.clone()));
    let engine = SyncEngine::spawn_with_emitter(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        Some(emitter),
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;
    // Let the confirm event flush onto the emitter.
    wait_until(
        || {
            events
                .lock()
                .unwrap()
                .iter()
                .any(|(n, p)| n == "sync-finished" && p["outcome"].as_str() == Some("confirmed"))
        },
        WAIT,
    )
    .await;

    let evts = events.lock().unwrap();
    let finished = evts.iter().find(|(n, _)| n == "sync-finished").expect("a sync-finished event");
    assert_eq!(finished.1["direction"].as_str(), Some("sent"));
    assert_eq!(finished.1["outcome"].as_str(), Some("confirmed"));
    assert_eq!(finished.1["okCount"].as_u64(), Some(1));
    assert!(
        evts.iter().any(|(n, p)| n == "sync-progress"
            && p["stage"].as_str() == Some("transferring")
            && p["direction"].as_str() == Some("sent")),
        "expected a coarse 'transferring' progress tick, got {evts:?}"
    );
    assert!(evts.len() <= 4, "coarse per-package events only, got {}: {evts:?}", evts.len());
    drop(evts);

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
            direction: None,
            peer: None,
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
            direction: None,
            peer: None,
            limit: 100,
        })
        .unwrap();
    assert!(
        history.iter().any(|h| h.outcome == "failed"),
        "a failed outcome must be recorded in history"
    );

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// Regression tests for the first-attempt "peer offline at send time" wedge
// (review findings C1 + M1).
// ---------------------------------------------------------------------------

/// C1: enqueue while the peer endpoint is NOT started. The very first announce
/// fails ("peer not started"); the engine must treat that as a retryable
/// attempt — retry (attempts climb) and terminalize `Failed` after
/// `max_attempts`, with a `failed` outcome in history — instead of leaving the
/// row wedged in `Queued` with no retry slot.
#[tokio::test]
async fn first_attempt_peer_offline_retries_then_fails() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Peer endpoint is minted (so we have a stable node id to announce *to*) but
    // never started → its mailbox is absent → every announce fails.
    let receiver = net.endpoint();
    let receiver_id = receiver.node_id();

    let pkg = build_package(&tmp.path().join("src6"), "uuid-6", "frame6.fits", "IC1396", 1024);

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

    // The peer never comes online: the row must reach terminal Failed.
    wait_until(|| state_of(&store, id) == Some(OutboundState::Failed), WAIT).await;

    let row = store.get_outbound(id).unwrap().unwrap();
    assert_eq!(row.state, OutboundState::Failed);
    assert_eq!(
        row.attempts, 5,
        "a first-attempt offline failure must be retried up to max_attempts, not wedged"
    );
    assert!(
        row.attempts >= 2,
        "retry machinery must have re-attempted (attempts observable >= 2)"
    );

    let history = store
        .search_history(HistoryQuery {
            filename: Some("frame6.fits".to_string()),
            object: None,
            direction: None,
            peer: None,
            limit: 100,
        })
        .unwrap();
    assert!(
        history.iter().any(|h| h.outcome == "failed"),
        "a failed outcome must be recorded in history"
    );

    engine.shutdown().await;
}

/// C1 companion: enqueue while the peer is offline (first announce fails), then
/// bring the peer online before `max_attempts` is exhausted. A retry's announce
/// then succeeds, the peer fetches + acks, and the row completes to `Confirmed`
/// with the correct two-event history.
#[tokio::test]
async fn first_attempt_peer_offline_then_online_completes() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Not started yet → offline. We keep the endpoint so we can start it later.
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.node_id();

    let pkg = build_package(&tmp.path().join("src7"), "uuid-7", "frame7.fits", "M27", 4096);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig {
            // Fast retry cadence, generous cap so the peer has time to come up.
            ack_timeout: Duration::from_millis(50),
            max_attempts: 20,
        },
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();

    // Prove it retried at least once while the peer was offline (each retry bumps
    // attempts; the initial failed attempt does not).
    wait_until(|| attempts_of(&store, id) >= 1, WAIT).await;
    assert_ne!(
        state_of(&store, id),
        Some(OutboundState::Confirmed),
        "must not be confirmed while the peer is still offline"
    );

    // Bring the peer online: register its mailbox and start acking.
    receiver.start().await.unwrap();
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    // The next retry's announce now lands → fetch → ack → Confirmed.
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
            filename: Some("frame7.fits".to_string()),
            object: None,
            direction: None,
            peer: None,
            limit: 100,
        })
        .unwrap();
    assert!(
        history
            .iter()
            .any(|h| h.finished_at.is_none() && h.outcome == "sent"),
        "expected a 'sent' start event once the announce finally lands"
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
            direction: None,
            peer: None,
            limit: 100,
        })
        .unwrap();
    assert!(
        history.iter().any(|h| h.outcome == "cancelled"),
        "a cancelled outcome must be recorded in history"
    );

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// Stage 1.5.1 Task 1: package payload cleanup on confirm + startup heal.
//
// `write_package` COPIES every source file into the package dir; nothing ever
// removed those copies, so confirmed packages kept a full duplicate of every
// frame forever. Cleanup on confirm (and a startup heal for crash-orphaned
// confirmed dirs) reclaims that space while KEEPING `manifest.ndjson` — the
// retention/audit trail reads it long after confirmation.
// ---------------------------------------------------------------------------

/// Test A: a package driven to `Confirmed` over loopback has its payload copies
/// removed, leaving ONLY `manifest.ndjson` in the package dir.
#[tokio::test]
async fn confirm_cleans_payloads_to_manifest_only() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let pkg = build_package(&tmp.path().join("srcA"), "uuid-a", "frameA.fits", "M42", 4096);
    // Pre-confirm the writer's payload copy exists inside the package dir.
    assert!(
        pkg.join("frameA.fits").exists(),
        "the writer must have copied the payload into the package dir"
    );

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;

    // Cleanup runs in the confirm path (after append_confirmed_history); poll.
    wait_until(|| dir_entries(&pkg) == vec![MANIFEST_FILENAME.to_string()], WAIT).await;

    assert_eq!(
        dir_entries(&pkg),
        vec![MANIFEST_FILENAME.to_string()],
        "confirm must leave ONLY the manifest in the package dir"
    );
    assert!(
        pkg.join(MANIFEST_FILENAME).exists(),
        "the manifest must survive cleanup for the retention/audit trail"
    );

    engine.shutdown().await;
}

/// Test B: startup heal. Seed the store with a `Confirmed` row whose package dir
/// still holds a payload + manifest (as if a prior engine confirmed it but
/// crashed before cleaning); spawning a fresh engine must clean the payload and
/// keep the manifest, and re-cleaning an already-clean dir stays a no-op.
#[tokio::test]
async fn startup_heal_cleans_confirmed_payloads() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;

    let pkg = build_package(&tmp.path().join("srcB"), "uuid-b", "frameB.fits", "M31", 8192);
    assert!(pkg.join("frameB.fits").exists());

    // Seed a confirmed outbound row pointing at the package dir.
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let id = store.enqueue(&pkg.to_string_lossy(), receiver_id).unwrap();
    store.confirm(id, &[]).unwrap();

    // Spawn the engine → its startup heal iterates confirmed() and cleans.
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
    );

    wait_until(|| !pkg.join("frameB.fits").exists(), WAIT).await;
    assert_eq!(
        dir_entries(&pkg),
        vec![MANIFEST_FILENAME.to_string()],
        "startup heal must leave ONLY the manifest"
    );
    assert!(
        pkg.join(MANIFEST_FILENAME).exists(),
        "the manifest must survive the heal"
    );

    engine.shutdown().await;

    // Idempotent: a second engine over an already-clean confirmed dir is a no-op
    // that never errors and never touches the manifest.
    let engine2 = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
    );
    // Give the heal a moment to run.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(dir_entries(&pkg), vec![MANIFEST_FILENAME.to_string()]);
    engine2.shutdown().await;
}

/// Test C: a package driven to terminal `Failed` KEEPS its payloads — Task 2's
/// retry re-enqueues the same package dir and depends on them.
#[tokio::test]
async fn failed_package_keeps_payloads() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Receiver is started (announce delivers) but never acks → every attempt
    // times out and the package terminalizes Failed.
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;

    let pkg = build_package(&tmp.path().join("srcC"), "uuid-c", "frameC.fits", "NGC7000", 1024);

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

    // A brief settle so any (erroneous) cleanup would have a chance to run.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        pkg.join("frameC.fits").exists(),
        "a failed package must KEEP its payloads (retry depends on them)"
    );
    assert!(pkg.join(MANIFEST_FILENAME).exists());

    engine.shutdown().await;
}
