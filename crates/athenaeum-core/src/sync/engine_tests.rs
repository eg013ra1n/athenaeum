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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::package::{
    self, write_package, ManifestRecord, PayloadKind, MANIFEST_FILENAME, MANIFEST_VERSION,
};
use crate::sharing::iroh::proto::{FullHashEntry, OfferEntry};
use crate::sharing::loopback::{FaultPlan, LoopbackNetwork, LoopbackTransport};
use crate::sharing::types::{
    FrameReceipt, NodeId, PackageAnnounce, PackageId, ReceiptOutcome, StartInfo, TransportEvent,
};
use crate::sharing::SharingTransport;

use super::cleanup_coord::SharedPackageCleanup;
use super::engine::{retry_backoff, PackageCleanupSink};
use super::store::{StandaloneSyncStore, SyncStore};
use super::DedupResponder;
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
        project: None,
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

/// Spawn a reactive receiver that fetches on every `AnnounceReceived` and acks
/// every manifest frame as `Cancelled` (Task 4) — the receiver deliberately
/// declines the whole package. Drives the sender's all-cancelled-ack terminal
/// path (a deliberate "no", distinct from a `Rejected` partial delivery).
fn spawn_cancelling_receiver(endpoint: Arc<LoopbackTransport>, dest_root: PathBuf) {
    tokio::spawn(async move {
        let mut events = endpoint.events().await;
        let mut n = 0usize;
        while let Some(event) = events.recv().await {
            let TransportEvent::AnnounceReceived { from, announce } = event else {
                continue;
            };
            n += 1;
            let dest = dest_root.join(format!("fetch-{n}"));
            if endpoint.fetch(from, &announce, &dest).await.is_ok() {
                let Ok(records) = package::read_manifest(&dest) else {
                    continue;
                };
                let receipts: Vec<FrameReceipt> = records
                    .iter()
                    .map(|r| FrameReceipt {
                        frame_uuid: r.frame_uuid.clone(),
                        xxh3: r.xxh3.clone(),
                        outcome: ReceiptOutcome::Cancelled,
                    })
                    .collect();
                let _ = endpoint.ack(from, &announce.package_id, receipts).await;
            }
        }
    });
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
            project: None,
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
            project: None,
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

#[test]
fn backoff_schedule_multiplies_base_and_caps() {
    use std::time::Duration;
    let base = Duration::from_secs(30);
    // 30s → 1m → 5m → 15m → 30m → 30m (cap), spec §2
    assert_eq!(retry_backoff(base, 0), Duration::from_secs(30));
    assert_eq!(retry_backoff(base, 1), Duration::from_secs(60));
    assert_eq!(retry_backoff(base, 2), Duration::from_secs(300));
    assert_eq!(retry_backoff(base, 3), Duration::from_secs(900));
    assert_eq!(retry_backoff(base, 4), Duration::from_secs(1800));
    assert_eq!(retry_backoff(base, 99), Duration::from_secs(1800));
}

/// Spec §2: a peer that never acks must NOT terminalize the package. Every ack
/// timeout climbs a backoff rung and waits it out — attempts keep rising, the
/// row stays non-terminal, `last_error` records the timeout, and NO `failed`
/// history row is ever written. Delivery is retried forever.
#[tokio::test]
async fn timeouts_back_off_forever_without_failing() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Receiver endpoint is started (so announce can be delivered) but has NO
    // reactive loop — it never acks, so every attempt's ack wait times out.
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;

    let pkg = build_package(&tmp.path().join("src4"), "uuid-4", "frame4.fits", "NGC7000", 1024);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig {
            ack_timeout: Duration::from_millis(50),
        },
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();

    // Drive past 5+ deadline fires. With a 50ms base the backoff climbs
    // 50ms → 100ms → 500ms → 1500ms → 3000ms, so reaching 5 announce attempts
    // takes ~5s of wall clock — hence the generous wait window.
    wait_until(|| attempts_of(&store, id) >= 5, Duration::from_secs(12)).await;

    // `last_error` records each ack timeout but is cleared by the next successful
    // announce, so wait for the ack-timeout record to land (it then persists for a
    // full backoff window) rather than racing the announce that just cleared it.
    wait_until(
        || {
            store
                .get_outbound(id)
                .unwrap()
                .map(|r| r.last_error.is_some())
                .unwrap_or(false)
        },
        WAIT,
    )
    .await;

    // Still non-terminal: it must appear in `non_terminal()`, and be neither
    // Failed nor Confirmed. Delivery is retried forever (spec §2).
    assert!(
        store.non_terminal().unwrap().iter().any(|r| r.id == id),
        "a peer that never acks must leave the row non-terminal"
    );
    let row = store.get_outbound(id).unwrap().unwrap();
    assert!(
        !matches!(row.state, OutboundState::Failed | OutboundState::Confirmed),
        "row must not terminalize from ack timeouts (state = {:?})",
        row.state
    );
    assert!(
        row.attempts >= 5,
        "attempts must keep climbing past 5, got {}",
        row.attempts
    );
    assert!(
        row.last_error.is_some(),
        "the ack timeout must be recorded as last_error"
    );

    let history = store
        .search_history(HistoryQuery {
            filename: Some("frame4.fits".to_string()),
            object: None,
            direction: None,
            peer: None,
            project: None,
            limit: 100,
        })
        .unwrap();
    assert!(
        !history.iter().any(|h| h.outcome == "failed"),
        "no `failed` history row may exist — timeouts never terminalize"
    );

    // Task 2: the wall-clock retry deadline is persisted so the UI can render a
    // live countdown. A successful re-announce clears it (now awaiting the ack),
    // so wait for a backoff window where it is armed AND parses to a future
    // instant — retrying past that transient clear.
    wait_until(
        || {
            store
                .non_terminal()
                .unwrap()
                .iter()
                .find(|r| r.id == id)
                .and_then(|r| r.next_retry_at.clone())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                .map(|t| t > chrono::Utc::now())
                .unwrap_or(false)
        },
        WAIT,
    )
    .await;

    // Receiver comes online: it fetches + acks the next re-announce, confirming
    // the package — which clears next_retry_at on the terminal transition.
    let _receiver = spawn_receiver(receiver.clone(), tmp.path().join("dest4"));
    wait_until(
        || state_of(&store, id) == Some(OutboundState::Confirmed),
        Duration::from_secs(12),
    )
    .await;
    assert_eq!(
        store.get_outbound(id).unwrap().unwrap().next_retry_at,
        None,
        "a confirmed package clears next_retry_at"
    );

    engine.shutdown().await;
}

/// Spec §1: `Failed` stays reachable for exactly ONE non-network case — the
/// package payload has vanished from disk, so re-announcing can never succeed no
/// matter how long the engine backs off. Unlike every other retry trigger (spec
/// §2), this one terminalizes instead of arming another backoff window.
#[tokio::test]
async fn missing_package_dir_fails_terminally() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Receiver endpoint is started (so the first announce delivers) but has NO
    // reactive loop — it never acks, so the ack wait times out and the engine
    // moves to the Retry backoff path, which is where the missing-dir check runs.
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;

    let pkg = build_package(&tmp.path().join("srcM"), "uuid-m", "frameM.fits", "M99", 1024);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig {
            ack_timeout: Duration::from_millis(50),
        },
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();

    // Let the first announce succeed (proves the normal path ran and the
    // manifest was read + cached) before pulling the dir out from under it.
    wait_until(
        || state_of(&store, id) == Some(OutboundState::Transferring),
        WAIT,
    )
    .await;

    // The payload vanishes from disk (e.g. cleaned up externally, moved,
    // corrupted parent). Re-announcing this package can never succeed again.
    std::fs::remove_dir_all(&pkg).unwrap();

    // The ack never comes, so the ack-wait deadline elapses, the engine backs
    // off into `Retry`, and the next `attempt()` finds the dir gone.
    wait_until(|| state_of(&store, id) == Some(OutboundState::Failed), WAIT).await;

    let row = store.get_outbound(id).unwrap().unwrap();
    assert_eq!(row.state, OutboundState::Failed);
    assert!(
        row.last_error
            .as_deref()
            .unwrap_or_default()
            .contains("missing"),
        "last_error must mention the missing payload, got {:?}",
        row.last_error
    );

    let history = store
        .search_history(HistoryQuery {
            filename: Some("frameM.fits".to_string()),
            object: None,
            direction: None,
            peer: None,
            project: None,
            limit: 100,
        })
        .unwrap();
    assert!(
        history.iter().any(|h| h.outcome == "failed"),
        "a failed outcome must be recorded in history even though the dir is gone"
    );

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// Regression tests for the first-attempt "peer offline at send time" wedge
// (review findings C1 + M1), now under delivery-forever semantics (spec §2).
// ---------------------------------------------------------------------------

/// Spec §2 / C1: enqueue while the peer endpoint is NOT started. Every announce
/// fails ("peer not started"); the engine must back off and keep retrying — the
/// row climbs backoff rungs and stays pending (`Queued`/`Announced`), never
/// `Failed`, no matter how many windows elapse. A network-unreachable peer is
/// never terminalized.
#[tokio::test]
async fn peer_offline_backs_off_and_stays_pending() {
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
            ack_timeout: Duration::from_millis(50),
        },
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();

    // Drive several backoff windows: each failed announce bumps attempts and
    // climbs a rung. The row must still be pending, never terminal.
    wait_until(|| attempts_of(&store, id) >= 3, WAIT).await;

    let row = store.get_outbound(id).unwrap().unwrap();
    assert!(
        matches!(row.state, OutboundState::Queued | OutboundState::Announced),
        "an offline peer must leave the row pending (Queued/Announced), got {:?}",
        row.state
    );
    assert_ne!(
        row.state,
        OutboundState::Failed,
        "a network-unreachable peer must never terminalize the package (spec §2)"
    );
    assert!(
        row.last_error.is_some(),
        "the serve/announce error must be recorded as last_error"
    );

    let history = store
        .search_history(HistoryQuery {
            filename: Some("frame6.fits".to_string()),
            object: None,
            direction: None,
            peer: None,
            project: None,
            limit: 100,
        })
        .unwrap();
    assert!(
        !history.iter().any(|h| h.outcome == "failed"),
        "no `failed` outcome may be recorded — offline peers back off forever"
    );

    engine.shutdown().await;
}

/// C1 companion: enqueue while the peer is offline (first announce fails), then
/// bring the peer online while it is still backing off and retrying. A retry's
/// announce then succeeds, the peer fetches + acks, and the row completes to
/// `Confirmed` with the correct two-event history.
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
            // Fast retry cadence so a backoff window lands soon after the peer
            // comes up.
            ack_timeout: Duration::from_millis(50),
        },
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();

    // Prove it attempted at least once while the peer was offline (every announce
    // attempt bumps the counter, including the first failing one).
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
            project: None,
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
async fn cancel_moves_to_cancelled_state() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Receiver started but not acking, so the package stays in flight until we
    // cancel it. Long ack timeout so a timeout does not race the cancel.
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;

    let pkg = build_package(&tmp.path().join("src5"), "uuid-5", "frame5.fits", "Sol", 1024);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    // Spawn WITH a capturing emitter so we can assert the `sync-finished` outcome
    // as well as the persisted terminal state.
    let events = Arc::new(std::sync::Mutex::new(Vec::<(String, serde_json::Value)>::new()));
    let emitter: Arc<dyn crate::events::ProgressEmitter> = Arc::new(CapturingEmitter(events.clone()));
    let engine = SyncEngine::spawn_with_config_and_emitter(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig {
            ack_timeout: Duration::from_secs(60),
        },
        Some(emitter),
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();
    wait_until(
        || state_of(&store, id) == Some(OutboundState::Transferring),
        WAIT,
    )
    .await;

    engine.cancel(id).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Cancelled), WAIT).await;

    // A user cancel lands in the first-class terminal `Cancelled` state, NOT
    // `Failed` — so the UI and retry eligibility can tell the two apart.
    assert_eq!(state_of(&store, id), Some(OutboundState::Cancelled));

    // History still records a `cancelled` outcome for the frame.
    let history = store
        .search_history(HistoryQuery {
            filename: Some("frame5.fits".to_string()),
            object: None,
            direction: None,
            peer: None,
            project: None,
            limit: 100,
        })
        .unwrap();
    assert!(
        history.iter().any(|h| h.outcome == "cancelled"),
        "a cancelled outcome must be recorded in history"
    );

    // The sender's single `sync-finished` event carries the `cancelled` outcome.
    wait_until(
        || {
            events
                .lock()
                .unwrap()
                .iter()
                .any(|(n, p)| n == "sync-finished" && p["outcome"].as_str() == Some("cancelled"))
        },
        WAIT,
    )
    .await;
    let evts = events.lock().unwrap();
    let finished = evts
        .iter()
        .find(|(n, _)| n == "sync-finished")
        .expect("a sync-finished event");
    assert_eq!(finished.1["outcome"].as_str(), Some("cancelled"));
    drop(evts);

    engine.shutdown().await;
}

/// Task 4: an ack whose receipts are non-empty and ALL `Cancelled` terminates the
/// outbound row as **cancelled-by-receiver** — NOT the normal confirm path. The
/// row lands in the first-class `Cancelled` terminal, carries the exact
/// `last_error` string the UI keys its "by receiver" display off, records a
/// per-frame `cancelled` history outcome, and emits a single `cancelled`
/// `sync-finished` event.
#[tokio::test]
async fn all_cancelled_ack_marks_cancelled_by_receiver() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    spawn_cancelling_receiver(receiver.clone(), tmp.path().join("recv"));

    let pkg = build_package(&tmp.path().join("src_cx"), "uuid-cx", "cx.fits", "M42", 2048);
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
    wait_until(|| state_of(&store, id) == Some(OutboundState::Cancelled), WAIT).await;

    // Terminal `Cancelled` carrying the exact "by receiver" reason string.
    let row = store.get_outbound(id).unwrap().unwrap();
    assert_eq!(row.state, OutboundState::Cancelled);
    assert_eq!(
        row.last_error.as_deref(),
        Some("cancelled by receiver"),
        "the UI keys its 'by receiver' display off this exact string"
    );
    // Cancelled, not confirmed — no confirmed_at stamp.
    assert!(row.confirmed_at.is_none(), "a cancelled row is never confirmed");

    // Per-frame history records a `cancelled` outcome (via the shared
    // receipts→history path with the new `Cancelled` mapping).
    let history = store
        .search_history(HistoryQuery {
            filename: Some("cx.fits".to_string()),
            object: None,
            direction: None,
            peer: None,
            project: None,
            limit: 100,
        })
        .unwrap();
    assert!(
        history.iter().any(|h| h.outcome == "cancelled"),
        "a cancelled history outcome must be recorded"
    );

    // The single `sync-finished` event carries the `cancelled` outcome.
    wait_until(
        || {
            events
                .lock()
                .unwrap()
                .iter()
                .any(|(n, p)| n == "sync-finished" && p["outcome"].as_str() == Some("cancelled"))
        },
        WAIT,
    )
    .await;
    let evts = events.lock().unwrap();
    let finished = evts
        .iter()
        .find(|(n, _)| n == "sync-finished")
        .expect("a sync-finished event");
    assert_eq!(finished.1["outcome"].as_str(), Some("cancelled"));
    drop(evts);

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

/// Test C: a package driven to a terminal, non-`Confirmed` state KEEPS its
/// payloads — only a `Confirmed` package's copies are reclaimed. Under
/// delivery-forever semantics (spec §2) a timeout never terminalizes, so the
/// terminal state here is reached by an explicit cancel (→ `Cancelled`).
#[tokio::test]
async fn cancelled_package_keeps_payloads() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Receiver started but not acking, so the package stays in flight until we
    // cancel it. Long ack timeout so a backoff pass does not race the cancel.
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;

    let pkg = build_package(&tmp.path().join("srcC"), "uuid-c", "frameC.fits", "NGC7000", 1024);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig {
            ack_timeout: Duration::from_secs(60),
        },
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();
    wait_until(
        || state_of(&store, id) == Some(OutboundState::Transferring),
        WAIT,
    )
    .await;

    // Cancel → terminal Cancelled.
    engine.cancel(id).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Cancelled), WAIT).await;

    // A brief settle so any (erroneous) cleanup would have a chance to run.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        pkg.join("frameC.fits").exists(),
        "a terminal non-confirmed package must KEEP its payloads (only confirm reclaims them)"
    );
    assert!(pkg.join(MANIFEST_FILENAME).exists());

    engine.shutdown().await;
}

/// Test D (Sync 2C): with a shared cleanup sink the confirming engine must NOT
/// delete the SHARED payload on its own confirm — it defers to the coordinator,
/// which cleans only once EVERY target is terminal. This is the fan-out
/// data-loss fix at the engine boundary (`spawn_with_sink` routes the confirmed
/// terminal through `on_terminal` instead of the in-line cleanup). A single
/// engine is driven to `Confirmed`; the coordinator is told two targets share
/// the dir, so the payload must survive until the second (simulated) target
/// terminalizes.
#[tokio::test]
async fn sink_defers_shared_payload_cleanup_until_all_targets_terminal() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let pkg = build_package(&tmp.path().join("srcD"), "uuid-d", "frameD.fits", "M42", 4096);
    assert!(pkg.join("frameD.fits").exists());

    // Two targets share this dir; only one is driven here.
    let coord = Arc::new(SharedPackageCleanup::new());
    coord.register(&pkg, 2);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn_with_sink(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        coord.clone() as Arc<dyn PackageCleanupSink>,
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;

    // Confirmed, but the shared payload MUST still be here — the second target
    // has not terminalized, so an in-line cleanup would have starved its retry.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        pkg.join("frameD.fits").exists(),
        "a sinked engine must NOT delete the shared payload on its own confirm"
    );

    // The second target reaches terminal → the coordinator cleans exactly once.
    coord.on_terminal(&pkg);
    assert_eq!(
        dir_entries(&pkg),
        vec![MANIFEST_FILENAME.to_string()],
        "once every target is terminal the coordinator leaves only the manifest"
    );

    engine.shutdown().await;
}

// ── M3: confirmation must be bound to the peer AND complete ───────────────────

/// M3 (peer-binding): an ack from a node other than the paired peer must not
/// confirm the package. The real peer receives the announce but never acks; a
/// *different* node forges an all-`Ingested` ack with the correct package_id.
/// The sender must ignore it — else a rogue node could drive a package to
/// `Confirmed` and let retention delete the capture originals.
#[tokio::test]
async fn ack_from_unexpected_peer_does_not_confirm() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let attacker = Arc::new(net.endpoint());
    attacker.start().await.unwrap();

    let sender_ep = Arc::new(net.endpoint());
    let sender_node = sender_ep.node_id();

    let pkg = build_package(&tmp.path().join("src"), "uuid-1", "f.fits", "M42", 1024);
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(store.clone() as Arc<dyn SyncStore>, sender_ep, receiver_id);

    let attacker_for_task = attacker.clone();
    tokio::spawn(async move {
        let mut events = receiver.events().await;
        while let Some(ev) = events.recv().await {
            if let TransportEvent::AnnounceReceived { announce, .. } = ev {
                let receipts = vec![FrameReceipt {
                    frame_uuid: "uuid-1".to_string(),
                    xxh3: "0".repeat(16),
                    outcome: ReceiptOutcome::Ingested,
                }];
                let _ = attacker_for_task
                    .ack(sender_node, &announce.package_id, receipts)
                    .await;
            }
        }
    });

    let id = engine.enqueue_package(&pkg).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_ne!(
        state_of(&store, id),
        Some(OutboundState::Confirmed),
        "an ack from a node other than the paired peer must not confirm the package"
    );

    engine.shutdown().await;
}

/// M3 (completeness): an ack that does not cover every announced frame must not
/// confirm. The paired peer acks with ZERO receipts (it stored nothing); the
/// sender must not treat that as a full delivery, else retention would delete an
/// un-transferred original.
#[tokio::test]
async fn empty_ack_does_not_confirm() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;

    let sender_ep = Arc::new(net.endpoint());
    let sender_node = sender_ep.node_id();

    let pkg = build_package(&tmp.path().join("src"), "uuid-1", "f.fits", "M42", 1024);
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(store.clone() as Arc<dyn SyncStore>, sender_ep, receiver_id);

    let receiver_for_task = receiver.clone();
    tokio::spawn(async move {
        let mut events = receiver_for_task.events().await;
        while let Some(ev) = events.recv().await {
            if let TransportEvent::AnnounceReceived { announce, .. } = ev {
                let _ = receiver_for_task
                    .ack(sender_node, &announce.package_id, vec![])
                    .await;
            }
        }
    });

    let id = engine.enqueue_package(&pkg).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_ne!(
        state_of(&store, id),
        Some(OutboundState::Confirmed),
        "an ack that does not cover every announced frame must not confirm"
    );

    engine.shutdown().await;
}

// ── Sync 2C (Task 7): shared store, per-target engines ────────────────────────

/// Crash-resume must be **peer-scoped**. Perseus fans one package out to N
/// per-target engines over a single shared `perseus.db`, so `non_terminal()`
/// returns rows for every peer. An engine bound to peer A that re-drove peer B's
/// row on startup would announce B's package to A (wrong peer) and let A's ack
/// confirm a row destined for B. This seeds two Queued rows (one per peer) into
/// one store, spawns ONE engine bound to peer A, and asserts only A's row is
/// re-driven to Confirmed while B's row is left untouched.
#[tokio::test]
async fn crash_resume_only_redrives_its_own_peers_rows() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Two receivers on one network: peer A acks everything; peer B is present
    // (so its id is dialable) but its row must never be driven by A's engine.
    let receiver_a = Arc::new(net.endpoint());
    let receiver_a_id = receiver_a.start().await.unwrap().node_id;
    let _stats_a = spawn_receiver(receiver_a.clone(), tmp.path().join("recv_a"));
    let receiver_b = Arc::new(net.endpoint());
    let receiver_b_id = receiver_b.start().await.unwrap().node_id;
    let _stats_b = spawn_receiver(receiver_b.clone(), tmp.path().join("recv_b"));

    let pkg_a = build_package(&tmp.path().join("src_a"), "uuid-a", "a.fits", "M42", 2048);
    let pkg_b = build_package(&tmp.path().join("src_b"), "uuid-b", "b.fits", "M31", 2048);

    // Seed both rows directly (as a prior multi-engine run would have left them),
    // each bound to its own peer.
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let id_a = store.enqueue(&pkg_a.to_string_lossy(), receiver_a_id).unwrap();
    let id_b = store.enqueue(&pkg_b.to_string_lossy(), receiver_b_id).unwrap();

    // ONE engine, bound to peer A only.
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_a_id,
    );

    wait_until(
        || state_of(&store, id_a) == Some(OutboundState::Confirmed),
        WAIT,
    )
    .await;

    // B's row was never A's to drive: it stays exactly as seeded (Queued),
    // never announced to A, never confirmed.
    assert_eq!(
        state_of(&store, id_b),
        Some(OutboundState::Queued),
        "an engine must not re-drive another peer's outbound row"
    );

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// Sync Phase 3 (Task 6): pre-announce dedup handshake spliced into the sender.
//
// The engine now computes an Offer, `negotiate_want`s with the peer, and either
// (a) empty want → terminalizes as all-duplicate WITHOUT announcing, (b)
// non-empty want → serves only that subset + announces, or (c) any handshake
// failure → falls back to announcing the FULL package (best-effort). The
// sender's `sync-finished` event carries a `{newCount, duplicateCount}` outcome.
// ---------------------------------------------------------------------------

/// Build a package with N frames under `src_root` and return its dir. Each
/// `(uuid, filename, object, size)` writes a real payload + a manifest record.
fn build_package_multi(
    src_root: &Path,
    pkg_name: &str,
    frames: &[(&str, &str, &str, usize)],
) -> PathBuf {
    std::fs::create_dir_all(src_root).unwrap();
    let mut items = Vec::new();
    for (frame_uuid, filename, object, size) in frames {
        let payload = src_root.join(filename);
        let bytes: Vec<u8> = (0..*size).map(|i| (i % 251) as u8).collect();
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
            project: None,
        };
        items.push((payload, record));
    }
    let pkg_dir = src_root.parent().unwrap().join(format!("pkg-{pkg_name}"));
    write_package(&pkg_dir, items).unwrap();
    pkg_dir
}

/// A [`DedupResponder`] that reports EVERY offered frame as a true duplicate:
/// nothing is a definite want, and every candidate's full hash "matches" (drop
/// them all) → the negotiated want is empty.
struct AllDuplicateResponder;
impl DedupResponder for AllDuplicateResponder {
    fn want_for_offer(&self, entries: &[OfferEntry]) -> (Vec<String>, Vec<String>) {
        (
            Vec::new(),
            entries.iter().map(|e| e.rel_path.clone()).collect(),
        )
    }
    fn confirm_full_hashes(&self, _entries: &[FullHashEntry]) -> Vec<String> {
        Vec::new()
    }
}

/// A [`DedupResponder`] that wants exactly one `rel_path` and treats every other
/// offered frame as a true duplicate (candidate whose full hash matches → drop).
struct WantOnlyResponder {
    wanted_rel: String,
}
impl DedupResponder for WantOnlyResponder {
    fn want_for_offer(&self, entries: &[OfferEntry]) -> (Vec<String>, Vec<String>) {
        let mut want = Vec::new();
        let mut cands = Vec::new();
        for e in entries {
            if e.rel_path == self.wanted_rel {
                want.push(e.rel_path.clone());
            } else {
                cands.push(e.rel_path.clone());
            }
        }
        (want, cands)
    }
    fn confirm_full_hashes(&self, _entries: &[FullHashEntry]) -> Vec<String> {
        Vec::new()
    }
}

/// A transport decorator that forces `negotiate_want` to error while delegating
/// every other call to the wrapped loopback endpoint — so the engine's
/// best-effort fallback (announce the FULL package on a handshake failure) can
/// be exercised end to end with a working serve/announce/fetch/ack path.
struct NegotiateErrTransport(Arc<LoopbackTransport>);

#[async_trait::async_trait]
impl SharingTransport for NegotiateErrTransport {
    async fn start(&self) -> anyhow::Result<StartInfo> {
        self.0.start().await
    }
    async fn announce(&self, to: NodeId, a: &PackageAnnounce) -> anyhow::Result<()> {
        self.0.announce(to, a).await
    }
    async fn fetch(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
    ) -> anyhow::Result<()> {
        self.0.fetch(from, pkg, dest_dir).await
    }
    async fn serve(
        &self,
        pkg: &PackageAnnounce,
        src_dir: &Path,
        want: Option<&HashSet<String>>,
    ) -> anyhow::Result<()> {
        self.0.serve(pkg, src_dir, want).await
    }
    async fn ack(
        &self,
        to: NodeId,
        package_id: &PackageId,
        receipts: Vec<FrameReceipt>,
    ) -> anyhow::Result<()> {
        self.0.ack(to, package_id, receipts).await
    }
    async fn negotiate_want(
        &self,
        _to: NodeId,
        _package_id: PackageId,
        _offer: Vec<OfferEntry>,
        _full_by_rel: HashMap<String, String>,
    ) -> anyhow::Result<HashSet<String>> {
        anyhow::bail!("injected negotiate failure")
    }
    async fn release(&self, package_id: &PackageId) -> anyhow::Result<()> {
        self.0.release(package_id).await
    }
    async fn events(&self) -> mpsc::Receiver<TransportEvent> {
        self.0.events().await
    }
}

/// All-duplicate: the peer reports every offered frame as a true duplicate, so
/// the negotiated want is empty. The engine must terminalize the package to
/// `Confirmed` WITHOUT announcing (the receiver never fetches), and the sender's
/// finished event must report `{ newCount: 0, duplicateCount: 1 }`.
#[tokio::test]
async fn all_duplicate_package_terminalizes_without_announce() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint_with_responder(Arc::new(AllDuplicateResponder)));
    let receiver_id = receiver.start().await.unwrap().node_id;
    let stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let pkg = build_package(&tmp.path().join("srcDup"), "uuid-dup", "dup.fits", "M42", 4096);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let events = Arc::new(std::sync::Mutex::new(Vec::<(String, serde_json::Value)>::new()));
    let emitter: Arc<dyn crate::events::ProgressEmitter> =
        Arc::new(CapturingEmitter(events.clone()));
    let engine = SyncEngine::spawn_with_emitter(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        Some(emitter),
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;

    // Let any (erroneous) announce reach the receiver before asserting none did.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        stats.attempts.load(SeqCst),
        0,
        "an all-duplicate package must never be announced to the peer"
    );

    wait_until(
        || {
            events
                .lock()
                .unwrap()
                .iter()
                .any(|(n, p)| n == "sync-finished" && p["outcome"] == "confirmed")
        },
        WAIT,
    )
    .await;
    let evts = events.lock().unwrap();
    let finished = evts
        .iter()
        .find(|(n, _)| n == "sync-finished")
        .expect("a sync-finished event");
    assert_eq!(finished.1["direction"].as_str(), Some("sent"));
    assert_eq!(finished.1["newCount"].as_u64(), Some(0));
    assert_eq!(finished.1["duplicateCount"].as_u64(), Some(1));
    drop(evts);

    engine.shutdown().await;
}

/// Best-effort fallback: when `negotiate_want` errors, the engine must announce
/// the FULL package (pre-dedup behavior) and the receiver ingests every frame →
/// `Confirmed`, finished `{ newCount: 1, duplicateCount: 0 }`.
#[tokio::test]
async fn negotiate_error_falls_back_to_full_announce() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let pkg = build_package(&tmp.path().join("srcErr"), "uuid-err", "err.fits", "M42", 4096);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let events = Arc::new(std::sync::Mutex::new(Vec::<(String, serde_json::Value)>::new()));
    let emitter: Arc<dyn crate::events::ProgressEmitter> =
        Arc::new(CapturingEmitter(events.clone()));
    // Wrap the sender endpoint so `negotiate_want` always errors → full send.
    let sender = Arc::new(NegotiateErrTransport(Arc::new(net.endpoint())));
    let engine = SyncEngine::spawn_with_emitter(
        store.clone() as Arc<dyn SyncStore>,
        sender,
        receiver_id,
        Some(emitter),
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;

    assert!(
        stats.attempts.load(SeqCst) >= 1,
        "a handshake failure must still announce the full package"
    );

    wait_until(
        || {
            events
                .lock()
                .unwrap()
                .iter()
                .any(|(n, p)| n == "sync-finished" && p["outcome"] == "confirmed")
        },
        WAIT,
    )
    .await;
    let evts = events.lock().unwrap();
    let finished = evts
        .iter()
        .find(|(n, _)| n == "sync-finished")
        .expect("a sync-finished event");
    assert_eq!(finished.1["newCount"].as_u64(), Some(1));
    assert_eq!(finished.1["duplicateCount"].as_u64(), Some(0));
    drop(evts);

    engine.shutdown().await;
}

/// Mixed batch: the peer wants 1 of 2 offered frames. The engine must serve only
/// that frame (the receiver fetches exactly it, never the duplicate), reach
/// `Confirmed`, and report finished `{ newCount: 1, duplicateCount: 1 }`.
#[tokio::test]
async fn mixed_batch_serves_only_want_subset() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // The peer already has "a.fits"; it wants only the new "b.fits".
    let responder = WantOnlyResponder {
        wanted_rel: "b.fits".to_string(),
    };
    let receiver = Arc::new(net.endpoint_with_responder(Arc::new(responder)));
    let receiver_id = receiver.start().await.unwrap().node_id;
    let recv_root = tmp.path().join("recv");
    let stats = spawn_receiver(receiver.clone(), recv_root.clone());

    let pkg = build_package_multi(
        &tmp.path().join("srcMix"),
        "uuid-mix",
        &[
            ("uuid-a", "a.fits", "M42", 2048),
            ("uuid-b", "b.fits", "M42", 4096),
        ],
    );

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let events = Arc::new(std::sync::Mutex::new(Vec::<(String, serde_json::Value)>::new()));
    let emitter: Arc<dyn crate::events::ProgressEmitter> =
        Arc::new(CapturingEmitter(events.clone()));
    let engine = SyncEngine::spawn_with_emitter(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        Some(emitter),
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;

    // Exactly one announce → exactly one fetch, of the wanted frame only.
    assert_eq!(
        stats.attempts.load(SeqCst),
        1,
        "a mixed batch must announce the subset exactly once"
    );
    let fetched = dir_entries(&recv_root.join("fetch-1"));
    assert!(
        fetched.contains(&"b.fits".to_string()),
        "the wanted frame must be fetched, got {fetched:?}"
    );
    assert!(
        !fetched.contains(&"a.fits".to_string()),
        "the duplicate frame must NOT be fetched, got {fetched:?}"
    );

    wait_until(
        || {
            events
                .lock()
                .unwrap()
                .iter()
                .any(|(n, p)| n == "sync-finished" && p["outcome"] == "confirmed")
        },
        WAIT,
    )
    .await;
    let evts = events.lock().unwrap();
    let finished = evts
        .iter()
        .find(|(n, _)| n == "sync-finished")
        .expect("a sync-finished event");
    assert_eq!(finished.1["newCount"].as_u64(), Some(1));
    assert_eq!(finished.1["duplicateCount"].as_u64(), Some(1));
    drop(evts);

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// Slice-4 collab exchange: project-aware announce + the request_project wire.
// ---------------------------------------------------------------------------

/// Build a one-frame package whose manifest carries a [`ProjectStamp`], so the
/// engine routes its announce through `announce_project` (slice 4).
fn build_project_package(
    src_root: &Path,
    frame_uuid: &str,
    filename: &str,
    project_id: &str,
    hub_package_id: &str,
) -> PathBuf {
    std::fs::create_dir_all(src_root).unwrap();
    let payload = src_root.join(filename);
    let bytes: Vec<u8> = (0..4096usize).map(|i| (i % 251) as u8).collect();
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
        frame_meta: serde_json::json!({ "filename": filename, "object": "M42" }),
        analysis: None,
        app_version: "test".to_string(),
        project: Some(crate::package::ProjectStamp {
            project_id: project_id.to_string(),
            package_id: hub_package_id.to_string(),
            thresholds_version: None,
            cal_engine_version: None,
        }),
    };
    let pkg_dir = src_root
        .parent()
        .unwrap()
        .join(format!("proj-pkg-{frame_uuid}"));
    write_package(&pkg_dir, vec![(payload, record)]).unwrap();
    pkg_dir
}

/// A project package (manifest carries a `ProjectStamp`) enqueued into the engine
/// is advertised to the peer as `ProjectAnnounceReceived` — carrying the hub id
/// and project id — NOT a plain `AnnounceReceived`.
#[tokio::test]
async fn project_package_announces_via_announce_project() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Receiver endpoint records the first inbound project advertisement.
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let seen: Arc<std::sync::Mutex<Option<TransportEvent>>> =
        Arc::new(std::sync::Mutex::new(None));
    {
        let receiver = receiver.clone();
        let seen = seen.clone();
        tokio::spawn(async move {
            let mut events = receiver.events().await;
            while let Some(ev) = events.recv().await {
                // A project package must never arrive as a plain announce.
                assert!(
                    !matches!(ev, TransportEvent::AnnounceReceived { .. }),
                    "project package must not arrive as a plain AnnounceReceived"
                );
                if matches!(ev, TransportEvent::ProjectAnnounceReceived { .. }) {
                    *seen.lock().unwrap() = Some(ev);
                    break;
                }
            }
        });
    }

    let pkg = build_project_package(
        &tmp.path().join("psrc"),
        "puuid-1",
        "p1.fits",
        "p-1",
        "hub-pkg-1",
    );

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
    );
    let _id = engine.enqueue_package(&pkg).await.unwrap();

    wait_until(|| seen.lock().unwrap().is_some(), WAIT).await;
    match seen.lock().unwrap().take().unwrap() {
        TransportEvent::ProjectAnnounceReceived {
            project_id,
            package_id,
            announce,
            ..
        } => {
            assert_eq!(project_id, "p-1");
            assert_eq!(package_id, "hub-pkg-1");
            assert_eq!(announce.frame_count, 1);
        }
        other => panic!("expected ProjectAnnounceReceived, got {other:?}"),
    }
    engine.shutdown().await;
}

/// `request_project` delivers a `ProjectRequestReceived` (carrying the hub id) to
/// the holder — the receive-role member's pull request over the loopback wire.
#[tokio::test]
async fn request_project_delivers_project_request_event() {
    let net = LoopbackNetwork::new();
    let holder = net.endpoint();
    holder.start().await.unwrap();
    let holder_id = holder.node_id();
    let mut events = holder.events().await;

    let member = net.endpoint();
    member.start().await.unwrap();
    member
        .request_project(holder_id, "p-1", "hub-pkg-1")
        .await
        .unwrap();

    match events.recv().await.unwrap() {
        TransportEvent::ProjectRequestReceived {
            project_id,
            package_id,
            ..
        } => {
            assert_eq!(project_id, "p-1");
            assert_eq!(package_id, "hub-pkg-1");
        }
        other => panic!("expected ProjectRequestReceived, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Task 8 (H2): the retry path re-resolves the peer's address and re-registers it.
// ---------------------------------------------------------------------------

/// A minimal transport that ACCEPTS every announce/serve but never delivers an
/// ack (its event stream stays open and silent), so every package's ack deadline
/// elapses and drives `handle_timeouts`. It records each `add_peer_addr` call so
/// the test can assert the engine re-registered the refresher's address.
struct RetryProbeTransport {
    node_id: NodeId,
    /// Retained so the engine's `events()` receiver never closes → no ack ever
    /// arrives → the ack deadline fires and the retry path runs.
    keep_tx: std::sync::Mutex<Option<mpsc::Sender<TransportEvent>>>,
    /// Every address the engine re-registered via `add_peer_addr` (T8).
    added: Arc<std::sync::Mutex<Vec<iroh::EndpointAddr>>>,
}

#[async_trait::async_trait]
impl SharingTransport for RetryProbeTransport {
    async fn start(&self) -> anyhow::Result<crate::sharing::types::StartInfo> {
        Ok(crate::sharing::types::StartInfo {
            node_id: self.node_id,
            pairing_ticket: String::new(),
        })
    }
    async fn announce(&self, _to: NodeId, _a: &PackageAnnounce) -> anyhow::Result<()> {
        Ok(())
    }
    async fn fetch(&self, _from: NodeId, _pkg: &PackageAnnounce, _dest: &Path) -> anyhow::Result<()> {
        Ok(())
    }
    async fn serve(
        &self,
        _pkg: &PackageAnnounce,
        _src: &Path,
        _want: Option<&HashSet<String>>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn ack(
        &self,
        _to: NodeId,
        _pid: &crate::sharing::types::PackageId,
        _r: Vec<FrameReceipt>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn release(&self, _pid: &crate::sharing::types::PackageId) -> anyhow::Result<()> {
        Ok(())
    }
    fn add_peer_addr(&self, addr: iroh::EndpointAddr) {
        self.added.lock().unwrap().push(addr);
    }
    async fn events(&self) -> mpsc::Receiver<TransportEvent> {
        let (tx, rx) = mpsc::channel(8);
        // Retain the sender so the receiver never closes and no ack is ever sent.
        *self.keep_tx.lock().unwrap() = Some(tx);
        rx
    }
}

/// (c) On a timed-out retry the engine awaits its address refresher and, on
///     `Some(addr)`, re-registers it via the transport — so a relay-map change or
///     the peer moving relays can't strand every retry on a dead cached path.
///     A counting refresher proves it is invoked; the probe transport records the
///     re-added address.
#[tokio::test]
async fn retry_path_re_resolves_and_re_adds_peer_address() {
    use std::sync::atomic::AtomicUsize;

    let tmp = tempdir().unwrap();
    // A VALID ed25519 peer id so the refresher can build a real `EndpointAddr`.
    let peer: NodeId = *iroh::SecretKey::generate().public().as_bytes();

    let added = Arc::new(std::sync::Mutex::new(Vec::<iroh::EndpointAddr>::new()));
    let transport = Arc::new(RetryProbeTransport {
        node_id: peer,
        keep_tx: std::sync::Mutex::new(None),
        added: Arc::clone(&added),
    });

    // A counting refresher that returns the peer's (freshly built) address.
    let refresh_count = Arc::new(AtomicUsize::new(0));
    let rc = Arc::clone(&refresh_count);
    let refresher: crate::sync::engine::AddrRefresher = Arc::new(move |p: NodeId| {
        let rc = Arc::clone(&rc);
        Box::pin(async move {
            rc.fetch_add(1, SeqCst);
            let id = iroh::EndpointId::from_bytes(&p).ok()?;
            Some(iroh::EndpointAddr::new(id))
        })
    });

    let pkg = build_package(&tmp.path().join("src-t8"), "uuid-t8", "f.fits", "M8", 1024);
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    // Short ack timeout so the ack deadline fires quickly and the backoff/retry
    // path runs. Delivery is retried forever (spec §2) — the retry path re-adding
    // the refreshed address is the observable signal, not a terminal state.
    let engine = SyncEngine::spawn_inner(
        store.clone() as Arc<dyn SyncStore>,
        transport as Arc<dyn SharingTransport>,
        peer,
        SyncConfig {
            ack_timeout: Duration::from_millis(80),
        },
        None,
        None,
        Some(refresher),
    );

    let id = engine.enqueue_package(&pkg).await.unwrap();
    // The ack never comes, so the ack deadline elapses and the Retry path runs,
    // re-resolving + re-registering the peer's address. Wait on that side effect
    // (the address landing on the transport) rather than a terminal state.
    wait_until(|| !added.lock().unwrap().is_empty(), WAIT).await;

    // The retry ran but the package is never terminalized from a network
    // condition — it stays non-terminal and keeps retrying (spec §2).
    assert!(
        !matches!(
            state_of(&store, id),
            Some(OutboundState::Failed | OutboundState::Confirmed)
        ),
        "a retrying package must stay non-terminal, got {:?}",
        state_of(&store, id)
    );

    assert!(
        refresh_count.load(SeqCst) >= 1,
        "the retry path must call the address refresher at least once"
    );
    let added = added.lock().unwrap();
    assert!(
        !added.is_empty(),
        "the engine must re-register the refreshed address on the transport"
    );
    assert_eq!(
        added[0].id.as_bytes(),
        &peer,
        "the re-added address must be the peer's refreshed address"
    );

    engine.shutdown().await;
}
