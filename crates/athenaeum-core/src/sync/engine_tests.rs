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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
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
use crate::sync::diagnostics::ConnectClass;
use crate::sharing::types::{
    AnnounceFileEntry, FrameReceipt, NodeId, PackageAnnounce, PackageId, ReceiptOutcome,
    RevokeReason, StartInfo, TransportEvent,
};
use crate::sharing::{noop_fetch_sink, FetchSink, SharingTransport};

use super::cleanup_coord::SharedPackageCleanup;
use super::engine::{retry_backoff, PackageCleanupSink, SyncEngineHandle};
use super::sender::{StartedSender, SyncSenderRuntime};
use super::store::{StandaloneSyncStore, SyncStore};
use super::DedupResponder;
use super::{Direction, HistoryQuery, OutboundFileState, OutboundState, SyncConfig, SyncEngine};

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
            let TransportEvent::AnnounceReceived { from, announce, .. } = event else {
                continue;
            };
            n += 1;
            a.fetch_add(1, SeqCst);
            let dest = dest_root.join(format!("fetch-{n}"));
            match endpoint.fetch(from, &announce, &dest, noop_fetch_sink()).await {
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
            let TransportEvent::AnnounceReceived { from, announce, .. } = event else {
                continue;
            };
            n += 1;
            let dest = dest_root.join(format!("fetch-{n}"));
            if endpoint.fetch(from, &announce, &dest, noop_fetch_sink()).await.is_ok() {
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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
            package_id: None,
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
    // Task 14: both the started + confirm rows carry the stable per-batch key —
    // the package dir's basename (`build_package` names it `pkg-<uuid>`) — so
    // `list_transfer_files` can recover it from the outbound row.
    assert!(
        history.iter().all(|h| h.package_id.as_deref() == Some("pkg-uuid-1")),
        "every sent history row is stamped with the package dir basename batch key"
    );

    engine.shutdown().await;
}

/// tv2 §D1 (item 5): the outbound row's `display_name` is threaded onto every
/// sent history row (`batch_name`) — so the transfer log shows the named batch on
/// the send side too — after a full confirmed loopback transfer.
#[tokio::test]
async fn sent_history_rows_carry_batch_name_after_confirm() {
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

    // Enqueue WITH a human batch name (the send path supplies it in production).
    let id = engine
        .enqueue_package(&pkg, Some("My M42 Batch".to_string()), Vec::new())
        .await
        .unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;

    let history = store
        .search_history(HistoryQuery {
            filename: Some("frame1.fits".to_string()),
            direction: Some(Direction::Sent),
            limit: 100,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(history.len(), 2, "start + confirm rows");
    assert!(
        history.iter().all(|h| h.batch_name.as_deref() == Some("My M42 Batch")),
        "every sent history row (started + confirmed) carries the batch name"
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
                let TransportEvent::AnnounceReceived { from, announce, .. } = event else {
                    continue;
                };
                *captured.lock().unwrap() = Some(announce.clone());
                n += 1;
                let dest = dest_root.join(format!("fetch-{n}"));
                if receiver.fetch(from, &announce, &dest, noop_fetch_sink()).await.is_ok() {
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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
            .fetch(sender_node, &captured_announce, dest.path(), noop_fetch_sink())
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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
    // Coarse per-package stage events only, never per-byte spam. The happy path
    // is: queued → transferring → transferring(bytes) → uploaded (Task 2.1's
    // "awaiting confirmation" tick) → confirmed = 5 discrete STAGE events, plus the
    // per-file `sync-file-progress` bars PER FILE (Task 2.2). A file emits a start
    // (bytes_done 0) and a completion tick (tv2 Task 4 mock-fidelity — iroh emits
    // real partial progress), so a single-file package here yields at most 2.
    // Assert the coarse stage events stay bounded and the per-file ticks stay
    // proportional to the file count (never per-byte spam).
    let stage_events = evts.iter().filter(|(n, _)| n != "sync-file-progress").count();
    let file_events = evts.iter().filter(|(n, _)| n == "sync-file-progress").count();
    assert!(
        stage_events <= 5,
        "coarse per-package stage events only, got {stage_events}: {evts:?}"
    );
    assert!(
        file_events <= 2,
        "bounded per-file bars for a single-file package (start + complete), got {file_events}: {evts:?}"
    );
    drop(evts);

    engine.shutdown().await;
}

/// Task 13: a served package's upload progress reaches the SENDER as a
/// `sync-progress` `transferring` tick carrying byte figures. Over the loopback
/// the serving endpoint receives one synthetic `ServeProgress` == the announced
/// byte size the moment the receiver fetches; the sender engine turns it into a
/// send-side progress tick with `bytesDone == bytesTotal == byte_size`. This is
/// the in-process stand-in for the iroh provider-upload-events path.
#[tokio::test]
async fn sent_progress_carries_bytes() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let pkg = build_package(&tmp.path().join("src_bytes"), "uuid-bytes", "bytes.fits", "M42", 4096);
    // The announced byte size is the manifest's summed frame bytes.
    let expected_bytes: u64 = package::read_manifest(&pkg)
        .unwrap()
        .iter()
        .map(|r| r.byte_size)
        .sum();
    assert!(expected_bytes > 0, "sanity: package has bytes");

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let events = Arc::new(std::sync::Mutex::new(Vec::<(String, serde_json::Value)>::new()));
    let emitter: Arc<dyn crate::events::ProgressEmitter> = Arc::new(CapturingEmitter(events.clone()));
    let engine = SyncEngine::spawn_with_emitter(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        Some(emitter),
    );

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;
    // Wait for the byte-bearing sent progress tick to flush onto the emitter.
    wait_until(
        || {
            events.lock().unwrap().iter().any(|(n, p)| {
                n == "sync-progress"
                    && p["direction"].as_str() == Some("sent")
                    && p["bytesDone"].as_u64() == Some(expected_bytes)
            })
        },
        WAIT,
    )
    .await;

    let evts = events.lock().unwrap();
    let tick = evts
        .iter()
        .find(|(n, p)| {
            n == "sync-progress" && p["direction"].as_str() == Some("sent") && p["bytesDone"].is_u64()
        })
        .expect("a send-side sync-progress tick carrying bytes");
    assert_eq!(tick.1["stage"].as_str(), Some("transferring"));
    assert_eq!(tick.1["bytesDone"].as_u64(), Some(expected_bytes));
    assert_eq!(tick.1["bytesTotal"].as_u64(), Some(expected_bytes));
    drop(evts);

    engine.shutdown().await;
}

/// Task 2.1: a successful loopback send surfaces the honest "uploaded — awaiting
/// confirmation" stage — a send-side `sync-progress` with `stage == "uploaded"`
/// — STRICTLY BEFORE the terminal `sync-finished{confirmed}`. The loopback mock
/// synthesizes a `ServeComplete` after a successful fetch (mirroring the iroh
/// provider's terminal `Completed` of a payload-carrying request), which the
/// sender engine folds into the uploaded tick with NO store write and NO state
/// transition; the receiver's ack (arriving after the fetch) is what actually
/// confirms. Ordering is the whole point: an operator must see the post-upload,
/// pre-ack window rather than a bar snapping to 100% while a batch is still
/// mid-flight.
#[tokio::test]
async fn sent_upload_complete_precedes_confirmed() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let pkg = build_package(&tmp.path().join("src_up"), "uuid-up", "up.fits", "M42", 4096);
    let expected_bytes: u64 = package::read_manifest(&pkg)
        .unwrap()
        .iter()
        .map(|r| r.byte_size)
        .sum();

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let events = Arc::new(std::sync::Mutex::new(Vec::<(String, serde_json::Value)>::new()));
    let emitter: Arc<dyn crate::events::ProgressEmitter> = Arc::new(CapturingEmitter(events.clone()));
    let engine = SyncEngine::spawn_with_emitter(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        Some(emitter),
    );

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;
    // Both signals must have landed on the emitter: the uploaded tick and the
    // confirmed finished event.
    wait_until(
        || {
            let evts = events.lock().unwrap();
            let uploaded = evts.iter().any(|(n, p)| {
                n == "sync-progress"
                    && p["direction"].as_str() == Some("sent")
                    && p["stage"].as_str() == Some("uploaded")
            });
            let confirmed = evts
                .iter()
                .any(|(n, p)| n == "sync-finished" && p["outcome"].as_str() == Some("confirmed"));
            uploaded && confirmed
        },
        WAIT,
    )
    .await;

    let evts = events.lock().unwrap();
    let uploaded_idx = evts
        .iter()
        .position(|(n, p)| {
            n == "sync-progress"
                && p["direction"].as_str() == Some("sent")
                && p["stage"].as_str() == Some("uploaded")
        })
        .expect("an 'uploaded' (awaiting-confirmation) send-side sync-progress tick");
    let confirmed_idx = evts
        .iter()
        .position(|(n, p)| n == "sync-finished" && p["outcome"].as_str() == Some("confirmed"))
        .expect("a confirmed sync-finished event");
    assert!(
        uploaded_idx < confirmed_idx,
        "the uploaded stage must be surfaced BEFORE the package confirms \
         (uploaded @ {uploaded_idx}, confirmed @ {confirmed_idx})"
    );
    // The uploaded tick reports the full byte size but is NOT a terminal event.
    let uploaded = &evts[uploaded_idx].1;
    assert_eq!(uploaded["bytesDone"].as_u64(), Some(expected_bytes));
    assert_eq!(uploaded["bytesTotal"].as_u64(), Some(expected_bytes));
    drop(evts);

    engine.shutdown().await;
}

/// Task 2.2: a successful loopback send surfaces per-FILE upload bars — a
/// send-side `sync-file-progress` event PER FILE, keyed by the outbound ROW id
/// (matching the outbound Active row key and the send-side `sync-progress`
/// convention) with `file` the full forward-slash `rel_path`, each reaching its
/// own size. The loopback mock synthesizes one `ServeFileProgress` per file
/// (mirroring the iroh provider's per-file, by-index attribution), which the
/// sender engine folds into `sync-file-progress`.
#[tokio::test]
async fn sent_per_file_upload_progress_keyed_by_row_id() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    // Two files of distinct sizes so each per-file bar's terminal is unambiguous.
    let pkg = build_package_multi(
        &tmp.path().join("src_files"),
        "pf",
        &[
            ("uuid-f1", "one.fits", "M42", 4096),
            ("uuid-f2", "two.fits", "M42", 8192),
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;

    let evts = events.lock().unwrap();
    let id_str = id.to_string();
    // Every send-side `sync-file-progress` must be keyed by the outbound ROW id
    // (`id.to_string()`), NOT any wire package id — this is what the outbound Active
    // row keys its live-file map on.
    let mut file_tick_count = 0;
    for (n, p) in evts.iter() {
        if n != "sync-file-progress" {
            continue;
        }
        file_tick_count += 1;
        assert_eq!(
            p["packageId"].as_str(),
            Some(id_str.as_str()),
            "send-side sync-file-progress must be keyed by the outbound row id: {p}"
        );
    }
    assert!(
        file_tick_count > 0,
        "at least one send-side sync-file-progress event: {evts:?}"
    );
    // Both files, by basename, reach their own size in a per-file tick.
    for (file, size) in [("one.fits", 4096u64), ("two.fits", 8192u64)] {
        let reached = evts.iter().any(|(n, p)| {
            n == "sync-file-progress"
                && p["file"].as_str() == Some(file)
                && p["bytesDone"].as_u64() == Some(size)
                && p["bytesTotal"].as_u64() == Some(size)
        });
        assert!(
            reached,
            "file {file} must reach its size ({size}) in a per-file tick: {evts:?}"
        );
    }
    drop(evts);

    engine.shutdown().await;
}

/// Final-review fix: the send-side `sync-file-progress` `file` field carries the
/// FULL forward-slash `rel_path`, NOT the basename. The frontend live overlay keys
/// the per-file bar by the full rel_path (`useTransferQueue` stores by `p.file`,
/// `FileTree` looks it up by `entry.relPath`), so a nested outbound file (a WBPP
/// object send with `camera_c/lights/L_001.fits`-style paths) only animates if the
/// sender emits the full path — reducing to `L_001.fits` would never match.
#[tokio::test]
async fn sent_per_file_upload_progress_emits_full_nested_rel_path() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    // A nested rel_path (the object/camera/lights tree a WBPP object send mints)
    // plus a flat sibling, so the assertion pins the full path, not a basename.
    let pkg = build_package_multi(
        &tmp.path().join("src_files"),
        "nested",
        &[
            ("uuid-n1", "camera_c/lights/L_001.fits", "M42", 4096),
            ("uuid-n2", "flat.fits", "M42", 2048),
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;

    let evts = events.lock().unwrap();
    // The nested file reaches its size in a per-file tick keyed by the FULL
    // forward-slash rel_path — a basename reduction (`L_001.fits`) would fail here.
    let nested_full = evts.iter().any(|(n, p)| {
        n == "sync-file-progress"
            && p["file"].as_str() == Some("camera_c/lights/L_001.fits")
            && p["bytesDone"].as_u64() == Some(4096)
            && p["bytesTotal"].as_u64() == Some(4096)
    });
    assert!(
        nested_full,
        "nested file must emit its full rel_path (not the basename) in a per-file tick: {evts:?}"
    );
    // And no tick may carry the reduced basename for the nested file.
    let basename_leaked = evts
        .iter()
        .any(|(n, p)| n == "sync-file-progress" && p["file"].as_str() == Some("L_001.fits"));
    assert!(
        !basename_leaked,
        "nested file must NOT be reduced to its basename in any tick: {evts:?}"
    );
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
    let id = engine_a.enqueue_package(&pkg, None, Vec::new()).await.unwrap();

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

/// Zombie-inbound fix (Part 1): the wire `package_id` is minted once per outbound
/// row and persisted, so a crash-resume re-announce carries the SAME `package_id`
/// as the pre-restart attempt. Before the fix `announce_for_dir` minted a fresh
/// uuid on every build, so a sender that restarted mid-transfer re-announced under
/// a new id and orphaned the receiver's `sync_inbound` row forever. Captures every
/// announce the loopback receiver sees across a kill+restart and asserts they all
/// carry one id; also asserts the id is persisted on the outbound row.
#[tokio::test]
async fn resume_reannounce_reuses_same_wire_package_id() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    // Abort the receiver's first fetch so the row rests in Transferring (no ack)
    // and we can kill + restart the sender before it confirms.
    receiver.set_fault(FaultPlan {
        abort_after_bytes: Some(64),
        ..Default::default()
    });

    // Record the wire package_id of every announce the receiver sees, and (on a
    // successful fetch) ack every frame as ingested.
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let receiver = receiver.clone();
        let seen = seen.clone();
        let dest_root = tmp.path().join("recv");
        tokio::spawn(async move {
            let mut events = receiver.events().await;
            let mut n = 0usize;
            while let Some(event) = events.recv().await {
                let TransportEvent::AnnounceReceived { from, announce, .. } = event else {
                    continue;
                };
                seen.lock().unwrap().push(announce.package_id.0.clone());
                n += 1;
                let dest = dest_root.join(format!("fetch-{n}"));
                if receiver.fetch(from, &announce, &dest, noop_fetch_sink()).await.is_ok() {
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

    let pkg = build_package(&tmp.path().join("srcw"), "uuid-w", "framew.fits", "M31", 4096);
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
    let id = engine_a.enqueue_package(&pkg, None, Vec::new()).await.unwrap();

    // First announce seen; the row rests Transferring (its first fetch aborted).
    wait_until(|| !seen.lock().unwrap().is_empty(), WAIT).await;
    wait_until(
        || state_of(&store_a, id) == Some(OutboundState::Transferring),
        WAIT,
    )
    .await;
    let first_wire = seen.lock().unwrap()[0].clone();
    assert_eq!(
        store_a.get_outbound(id).unwrap().unwrap().wire_package_id.as_deref(),
        Some(first_wire.as_str()),
        "the wire package_id must be persisted on the outbound row on the first build"
    );

    // Kill the engine mid-flight; restart over the SAME store file with a fresh
    // transport endpoint (crash-resume).
    drop(engine_a);
    let store_b = Arc::new(StandaloneSyncStore::open(&db_path).unwrap());
    let engine_b = SyncEngine::spawn(
        store_b.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
    );

    // The resume re-announces; a second announce is seen and the package confirms.
    wait_until(|| seen.lock().unwrap().len() >= 2, WAIT).await;
    wait_until(
        || state_of(&store_b, id) == Some(OutboundState::Confirmed),
        WAIT,
    )
    .await;

    let all = seen.lock().unwrap().clone();
    assert!(all.len() >= 2, "resume should have re-announced; saw {all:?}");
    assert!(
        all.iter().all(|w| *w == first_wire),
        "every announce pre- and post-restart must carry the SAME wire package_id; saw {all:?}"
    );

    engine_b.shutdown().await;
}

/// Zombie-inbound fix (Part 1, the deliberate constraint): a re-enqueue of the
/// SAME package dir is a NEW outbound row with no persisted wire id, so it mints a
/// FRESH wire `package_id` — the id is NEVER derived from the dir. This is what
/// makes a resend (e.g. after the receiver deleted the files) transfer anew
/// instead of replay-confirming from stale receiver receipts keyed by the wire id.
#[tokio::test]
async fn reenqueue_same_dir_mints_a_new_wire_package_id() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let pkg = build_package(&tmp.path().join("srcr"), "uuid-r", "framer.fits", "M42", 2048);
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
    );

    let wire_of = |id: i64| store.get_outbound(id).unwrap().unwrap().wire_package_id;

    // First enqueue: a wire id is minted + persisted during the announce build.
    let id1 = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
    wait_until(|| wire_of(id1).is_some(), WAIT).await;
    let wire1 = wire_of(id1).unwrap();

    // Re-enqueue the SAME dir → a DISTINCT row that mints its own fresh id.
    let id2 = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
    assert_ne!(id1, id2, "a re-enqueue is a distinct outbound row");
    wait_until(|| wire_of(id2).is_some(), WAIT).await;
    let wire2 = wire_of(id2).unwrap();

    assert_ne!(
        wire1, wire2,
        "a re-enqueue of the same dir mints a fresh wire package_id, never derived from the dir"
    );

    engine.shutdown().await;
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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
            package_id: None,
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

/// D1 §3.4: while the peer is absent exactly ONE package probes. Otherwise a
/// three-package queue against a shut laptop dials three times every window — and
/// a fifty-package queue fifty times, each paying a full payload re-hash.
/// A presence beacon then releases the whole queue at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn absent_peer_probes_with_one_package_then_presence_releases_all() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();
    // Minted but not started → every announce fails as `not_started`, an absent
    // class.
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.node_id();

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync_coalesce.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        // 700ms base -> a 2.8s flat window. Long enough that the delivery assertion
        // below (well under a second after the beacon) CANNOT be satisfied by the
        // head's own next probe — otherwise the presence half of this test would
        // pass with the beacon removed entirely.
        SyncConfig { ack_timeout: Duration::from_millis(700) },
    );

    let mut ids = Vec::new();
    for tag in ["a", "b", "c"] {
        let pkg = build_package(
            &tmp.path().join(format!("src_{tag}")),
            &format!("uuid-{tag}"),
            &format!("frame_{tag}.fits"),
            "M101",
            1024,
        );
        ids.push(engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap());
    }

    // Every package makes its OWN first attempt — an enqueue is user intent and
    // the peer state may be stale — and then parks behind the head.
    wait_until(|| ids.iter().all(|id| attempts_of(&store, *id) >= 1), WAIT).await;
    // One flat window (700ms * 4 = 2.8s) plus slack: the head probes again, the
    // parked pair must not.
    tokio::time::sleep(Duration::from_millis(3200)).await;

    let counts: Vec<u32> = ids.iter().map(|id| attempts_of(&store, *id)).collect();
    let head_attempts = counts[0];
    assert!(
        head_attempts >= 2,
        "the head keeps probing the absent peer, got {counts:?}"
    );
    assert_eq!(
        (counts[1], counts[2]),
        (1, 1),
        "the queue-mates park after their first attempt instead of dialing too: {counts:?}"
    );

    // The peer comes to life and announces itself: everything parked goes — and it
    // goes NOW, not on the head's next 2.8s probe. The tight bound is what makes
    // this half of the test fail if the beacon stops releasing the queue.
    receiver.start().await.unwrap();
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv_coalesce"));
    let beacon_at = tokio::time::Instant::now();
    engine.peer_present().await.unwrap();

    for id in &ids {
        wait_until(
            || state_of(&store, *id) == Some(OutboundState::Confirmed),
            Duration::from_millis(1200),
        )
        .await;
    }
    assert!(
        beacon_at.elapsed() < Duration::from_millis(2000),
        "the queue drained on the beacon, not on the head's next flat probe"
    );
    engine.shutdown().await;
}

/// Review finding (critical): ordinary contact with the peer must NOT reset the
/// ladder of a sibling whose ack merely timed out.
///
/// The everyday multi-batch shape: two packages to one peer, both announced; the
/// receiver ingests serially, so while it pulls A, B's ack times out and arms an
/// escalating retry. Every serve tick for A calls `peer_reachable` — and before
/// this rule it zeroed B's rung and collapsed its deadline, so B re-announced once
/// per ack_timeout forever at a peer that is demonstrably up and saturated, which
/// is exactly what the escalating schedule exists to prevent.
#[test]
fn ordinary_contact_releases_only_what_the_absence_parked() {
    use crate::sync::engine::{ids_to_release, ReachabilityProof};
    let pending = [1i64, 2, 3];
    // 2 was parked by an absence; 3 is armed by its own ack timeout.
    let parked: HashSet<i64> = [2i64].into_iter().collect();

    assert_eq!(
        ids_to_release(ReachabilityProof::Contact, &pending, &parked),
        vec![2],
        "a serve tick / announce / ack releases the absence-parked package only"
    );
}

/// The beacon is the other half of the rule: a peer that just announced itself has
/// restarted, so an ack-timeout-parked package — the receiver died mid-ingest,
/// audit F7 — is released too. Spec §6 makes the beacon its ONLY rescue.
#[test]
fn a_beacon_releases_every_armed_package_including_ack_timeouts() {
    use crate::sync::engine::{ids_to_release, ReachabilityProof};
    let pending = [1i64, 2, 3];
    let parked: HashSet<i64> = [2i64].into_iter().collect();

    assert_eq!(
        ids_to_release(ReachabilityProof::Beacon, &pending, &parked),
        vec![1, 2, 3],
        "a presence beacon releases the whole armed queue"
    );
}

/// The head is re-elected when it leaves `pending`, so a queue whose probing/// The head is re-elected when it leaves `pending`, so a queue whose probing
/// package terminalizes does not go silent forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn absent_head_is_re_elected_when_it_terminalizes() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();
    let receiver_id = net.endpoint().node_id();

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync_head.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig { ack_timeout: Duration::from_millis(150) },
    );

    // NOTE: `build_package` returns the PACKAGE dir (`pkg-<uuid>`, a sibling of the
    // source root) — that is what the engine serves and what must vanish to
    // terminalize the package. Removing the source root would leave the engine
    // happily re-announcing an intact package.
    let pkg_head = build_package(&tmp.path().join("src_head"), "uuid-head", "head.fits", "M101", 1024);
    let pkg_tail = build_package(&tmp.path().join("src_tail"), "uuid-tail", "tail.fits", "M101", 1024);
    let head = engine.enqueue_package(&pkg_head, None, Vec::new()).await.unwrap();
    let tail = engine.enqueue_package(&pkg_tail, None, Vec::new()).await.unwrap();

    wait_until(
        || attempts_of(&store, head) >= 1 && attempts_of(&store, tail) >= 1,
        WAIT,
    )
    .await;
    let tail_parked_at = attempts_of(&store, tail);

    // Terminalize the head the one way delivery-forever allows: its payload is
    // gone from disk, so re-announcing can never succeed (spec §1).
    std::fs::remove_dir_all(&pkg_head).unwrap();
    wait_until(|| state_of(&store, head) == Some(OutboundState::Failed), WAIT).await;

    // The tail must now BE the head: its attempts start climbing again.
    wait_until(|| attempts_of(&store, tail) > tail_parked_at, WAIT).await;
    engine.shutdown().await;
}

/// Presence must not disturb a live pull: a package awaiting its ack while the
/// peer is actively fetching keeps its deadline and issues no re-announce.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presence_does_not_re_announce_a_package_awaiting_ack() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();
    // Started receiver that never acks: the package reaches AwaitAck and stays
    // there, which is exactly the "mid-pull" shape presence must not disturb.
    let receiver = Arc::new(net.endpoint());
    receiver.start().await.unwrap();
    let receiver_id = receiver.node_id();

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync_await.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        // Long ack timeout: the package must still be awaiting its ack when the
        // beacon lands, not already backed off.
        SyncConfig { ack_timeout: Duration::from_secs(30) },
    );

    let pkg = build_package(&tmp.path().join("src_await"), "uuid-await", "await.fits", "M101", 1024);
    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
    wait_until(|| attempts_of(&store, id) >= 1, WAIT).await;
    let attempts_before = attempts_of(&store, id);

    engine.peer_present().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        attempts_of(&store, id),
        attempts_before,
        "a package awaiting its ack must not be re-announced by a presence beacon"
    );
    engine.shutdown().await;
}

#[test]
fn backoff_schedule_multiplies_base_and_caps() {
    use std::time::Duration;
    let base = Duration::from_secs(30);
    // 30s → 1m → 5m → 15m → 30m → 30m (cap), spec §2. `None` is the historical
    // schedule: no failed dial to classify (an ack timeout), so nothing says the
    // peer is absent.
    assert_eq!(retry_backoff(base, 0, None), Duration::from_secs(30));
    assert_eq!(retry_backoff(base, 1, None), Duration::from_secs(60));
    assert_eq!(retry_backoff(base, 2, None), Duration::from_secs(300));
    assert_eq!(retry_backoff(base, 3, None), Duration::from_secs(900));
    assert_eq!(retry_backoff(base, 4, None), Duration::from_secs(1800));
    assert_eq!(retry_backoff(base, 99, None), Duration::from_secs(1800));
}

/// D1: a peer that is not there gets a FLAT schedule. Escalation exists to spare
/// a loaded peer, and an absent peer is not loaded — climbing to the 30-minute cap
/// only means an overnight outage costs half an hour of idleness after the peer is
/// already back.
#[test]
fn absent_classes_retry_flat_and_never_escalate() {
    use std::time::Duration;
    let base = Duration::from_secs(30);
    // `not_started` is here on purpose: its doc covers "local OR remote endpoint
    // not started", and the remote reading IS an absent peer. A local one is
    // transient — our own start fires the wake hook long before the flat interval
    // matters.
    for class in [
        ConnectClass::NoRoute,
        ConnectClass::Timeout,
        ConnectClass::RelayUnreachable,
        ConnectClass::NotStarted,
    ] {
        for rung in 0..6 {
            assert_eq!(
                retry_backoff(base, rung, Some(class)),
                Duration::from_secs(120),
                "{class:?} at rung {rung} must stay flat at 4x the base"
            );
        }
    }
}

/// The flat interval is a MULTIPLE of the base, like every other rung — so an
/// engine configured with a millisecond timeout (every loopback test) observes the
/// flat path in milliseconds instead of waiting two minutes.
#[test]
fn the_flat_interval_scales_with_the_configured_base() {
    use std::time::Duration;
    assert_eq!(
        retry_backoff(Duration::from_millis(50), 3, Some(ConnectClass::NotStarted)),
        Duration::from_millis(200)
    );
}

/// A peer that is UP and declining us is a different fact from a peer that is
/// gone: it needs a human, so it keeps the escalating schedule.
#[test]
fn a_refusing_peer_still_escalates() {
    use std::time::Duration;
    let base = Duration::from_secs(30);
    assert_eq!(retry_backoff(base, 0, Some(ConnectClass::Refused)), Duration::from_secs(30));
    assert_eq!(retry_backoff(base, 1, Some(ConnectClass::Refused)), Duration::from_secs(60));
    assert_eq!(retry_backoff(base, 4, Some(ConnectClass::Refused)), Duration::from_secs(1800));
    // As does an unclassifiable failure — we do not know it is the peer's fault.
    assert_eq!(retry_backoff(base, 2, Some(ConnectClass::Other)), Duration::from_secs(300));
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();

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
            package_id: None,
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

// ---------------------------------------------------------------------------
// Log-assert (audit TEST-11): the ack-timeout backoff `info!` line must carry
// `delay_ms` (the just-computed backoff window) and `next_retry_at` (the
// wall-clock stamp reused from persistence) so an owner smoke reads the retry
// schedule with a one-line `query_logs` instead of watching the UI countdown.
// A minimal thread-local capture layer (mirrors the `sharing::iroh::tests`
// pattern) scoped to `athenaeum_core::sync` targets; `#[tokio::test]` is a
// current-thread runtime, so the engine worker task runs on this thread and its
// events are captured by the `set_default` subscriber.
// ---------------------------------------------------------------------------

/// One captured tracing event: its message plus its string-rendered fields.
#[derive(Clone, Default)]
struct CapturedEvent {
    message: String,
    fields: HashMap<String, String>,
}

/// Field visitor: records `message` separately, every other field as a string.
#[derive(Default)]
struct FieldCollector {
    message: String,
    fields: HashMap<String, String>,
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
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields.insert(field.name().to_string(), value.to_string());
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.insert(field.name().to_string(), value.to_string());
        }
    }
}

/// Minimal `tracing` layer capturing this crate's `sync` events into a shared vec.
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
        if !event.metadata().target().starts_with("athenaeum_core::sync") {
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

/// TEST-11: the `"ack timeout, backing off"` info line carries `delay_ms` and
/// `next_retry_at`, and the stamp is a parseable RFC3339 instant.
#[tokio::test]
async fn ack_timeout_backoff_log_carries_delay_and_next_retry() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let captured: Arc<std::sync::Mutex<Vec<CapturedEvent>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let _guard = tracing_subscriber::registry()
        .with(CaptureLayer {
            events: captured.clone(),
        })
        .set_default();

    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Receiver is started (so the announce delivers) but never acks — every
    // attempt's ack wait times out, firing the backoff info line.
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;

    let pkg = build_package(&tmp.path().join("src_log"), "uuid-log", "log.fits", "M31", 1024);
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig {
            ack_timeout: Duration::from_millis(50),
        },
    );
    let _id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();

    // Wait until at least one backoff line has been captured.
    wait_until(
        || {
            captured
                .lock()
                .unwrap()
                .iter()
                .any(|e| e.message == "ack timeout, backing off")
        },
        Duration::from_secs(5),
    )
    .await;

    let events = captured.lock().unwrap();
    let backoff: Vec<&CapturedEvent> = events
        .iter()
        .filter(|e| e.message == "ack timeout, backing off")
        .collect();
    assert!(
        !backoff.is_empty(),
        "expected an 'ack timeout, backing off' line; captured: {:?}",
        events.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
    );
    for e in &backoff {
        assert!(
            e.fields.contains_key("delay_ms"),
            "backoff line must carry delay_ms; got fields {:?}",
            e.fields
        );
        let stamp = e
            .fields
            .get("next_retry_at")
            .expect("backoff line must carry next_retry_at");
        assert!(
            chrono::DateTime::parse_from_rfc3339(stamp).is_ok(),
            "next_retry_at must be a parseable RFC3339 instant, got {stamp:?}"
        );
    }
    drop(events);

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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();

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
            package_id: None,
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();

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
            package_id: None,
            limit: 100,
        })
        .unwrap();
    assert!(
        !history.iter().any(|h| h.outcome == "failed"),
        "no `failed` outcome may be recorded — offline peers back off forever"
    );

    engine.shutdown().await;
}

/// Task 3.1: a failed serve/announce leaves a class-prefixed `last_error`. With
/// the peer endpoint minted but never started, every announce fails with a "peer
/// not started" chain, which classifies as `not_started` — so the persisted
/// `last_error` must carry the stable `not_started: ` machine-readable prefix a
/// later UI task maps to human text.
#[tokio::test]
async fn failed_announce_last_error_carries_not_started_class_prefix() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Peer endpoint minted (stable node id to announce *to*) but never started →
    // every serve/announce attempt fails with "peer not started".
    let receiver = net.endpoint();
    let receiver_id = receiver.node_id();

    let pkg = build_package(&tmp.path().join("src_cls"), "uuid-cls", "cls.fits", "M64", 1024);

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig {
            ack_timeout: Duration::from_millis(50),
        },
    );

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();

    // Wait for a failed attempt to record its classified last_error.
    wait_until(
        || {
            store
                .get_outbound(id)
                .unwrap()
                .and_then(|r| r.last_error)
                .is_some()
        },
        WAIT,
    )
    .await;

    let last_error = store
        .get_outbound(id)
        .unwrap()
        .unwrap()
        .last_error
        .expect("a failed announce must record last_error");
    assert!(
        last_error.starts_with("not_started: "),
        "last_error must carry the classified prefix, got {last_error:?}"
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();

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
            package_id: None,
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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
            package_id: None,
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
///
/// It must ALSO **keep its payload copies** — spec §1 ("the row stays in the
/// sender's history and is retryable") + §4 (retry checks payload presence)
/// require a receiver-cancelled package to be resendable via
/// `retry_sync_package`, which is only possible if the payload is still on disk.
/// This branch is the all-cancelled-ack terminal epilogue and must mirror
/// `cancel_package` exactly: only a `Confirmed` package's copies are ever
/// reclaimed (see `cancelled_package_keeps_payloads`, the sibling terminal
/// state this one joins; the `Failed` terminal has no keep-payloads twin —
/// it is only ever the vanished-payload path, with no copies left to keep).
/// Regression coverage for a real bug where an inline
/// `cleanup_package_payloads` call here broke resend.
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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

    // A brief settle so any (erroneous) cleanup would have a chance to run —
    // same idiom as `cancelled_package_keeps_payloads`.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        pkg.join("cx.fits").exists(),
        "an all-cancelled-ack (receiver-decline) package must KEEP its payload — \
         only a Confirmed package's copies are reclaimed, so retry_sync_package \
         can resend it (spec §1/§4)"
    );
    assert!(pkg.join(MANIFEST_FILENAME).exists());

    // Per-frame history records a `cancelled` outcome (via the shared
    // receipts→history path with the new `Cancelled` mapping).
    let history = store
        .search_history(HistoryQuery {
            filename: Some("cx.fits".to_string()),
            object: None,
            direction: None,
            peer: None,
            project: None,
            package_id: None,
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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
    let id = store.enqueue(&pkg.to_string_lossy(), receiver_id, None, &[]).unwrap();
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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
    let id_a = store.enqueue(&pkg_a.to_string_lossy(), receiver_a_id, None, &[]).unwrap();
    let id_b = store.enqueue(&pkg_b.to_string_lossy(), receiver_b_id, None, &[]).unwrap();

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
        // `filename` doubles as the manifest `rel_path`, so a nested rel_path
        // (`camera_c/lights/L_001.fits`) needs its parent dirs on disk before write.
        if let Some(parent) = payload.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
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

/// T3: the personal-sync announce carries the batch's REAL name (the outbound
/// row's `display_name`) and the REAL file manifest (one `AnnounceFileEntry` per
/// manifest record), not the T1 empty placeholder. Pre-seeds a NAMED `Queued`
/// row so crash-resume drives it deterministically — sidestepping the
/// enqueue→announce race for the name read.
#[tokio::test]
async fn announce_carries_batch_name_and_file_manifest() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Started receiver that captures the FIRST announce it sees.
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    type Captured = (Option<String>, Option<Vec<AnnounceFileEntry>>);
    let captured: Arc<std::sync::Mutex<Option<Captured>>> = Arc::new(std::sync::Mutex::new(None));
    {
        let receiver = receiver.clone();
        let captured = captured.clone();
        tokio::spawn(async move {
            let mut events = receiver.events().await;
            while let Some(event) = events.recv().await {
                if let TransportEvent::AnnounceReceived { batch_name, files, .. } = event {
                    *captured.lock().unwrap() = Some((batch_name, files));
                    break;
                }
            }
        });
    }

    // A two-file package + a pre-seeded, NAMED Queued row over the same store.
    let pkg = build_package_multi(
        &tmp.path().join("src"),
        "named",
        &[("uuid-a", "a.fits", "M42", 2048), ("uuid-b", "b.fits", "M42", 4096)],
    );
    // `CatalogSyncStore` (over a fresh db) exposes `lock_conn`, so the pre-seed
    // can set the row's `display_name` directly. Its `open` materialises the same
    // sync tables the standalone store does.
    let store = Arc::new(super::store::CatalogSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let id = store.enqueue(&pkg.to_string_lossy(), receiver_id, None, &[]).unwrap();
    crate::sync::store::set_outbound_display_name(&store.lock_conn(), id, Some("Orion Nebula"))
        .unwrap();

    // Spawn the engine over the SAME store — crash-resume drives the named row.
    let _engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()) as Arc<dyn SharingTransport>,
        receiver_id,
    );

    wait_until(|| captured.lock().unwrap().is_some(), WAIT).await;
    let (batch_name, files) = captured
        .lock()
        .unwrap()
        .clone()
        .expect("an announce was captured");

    assert_eq!(
        batch_name.as_deref(),
        Some("Orion Nebula"),
        "announce carries the row's display_name"
    );
    let files = files.expect("v2 announce carries a file manifest");
    let expected = package::read_manifest(&pkg).unwrap();
    assert_eq!(files.len(), expected.len(), "one entry per manifest record");
    let got: HashSet<(String, u64, String)> = files
        .into_iter()
        .map(|f| (f.rel_path, f.byte_size, f.frame_uuid))
        .collect();
    let want: HashSet<(String, u64, String)> = expected
        .into_iter()
        .map(|r| (r.rel_path, r.byte_size, r.frame_uuid))
        .collect();
    assert_eq!(got, want, "announce files mirror the package manifest");
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
    async fn announce(
        &self,
        to: NodeId,
        a: &PackageAnnounce,
        batch_name: &str,
        batch_uuid: &str,
        files: &[AnnounceFileEntry],
    ) -> anyhow::Result<()> {
        self.0.announce(to, a, batch_name, batch_uuid, files).await
    }
    async fn fetch(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
        sink: FetchSink,
    ) -> anyhow::Result<()> {
        self.0.fetch(from, pkg, dest_dir, sink).await
    }
    async fn fetch_manifest(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
    ) -> anyhow::Result<PathBuf> {
        self.0.fetch_manifest(from, pkg, dest_dir).await
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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
    let _id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();

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
    async fn announce(
        &self,
        _to: NodeId,
        _a: &PackageAnnounce,
        _batch_name: &str,
        _batch_uuid: &str,
        _files: &[AnnounceFileEntry],
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn fetch(
        &self,
        _from: NodeId,
        _pkg: &PackageAnnounce,
        _dest: &Path,
        _sink: FetchSink,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn fetch_manifest(
        &self,
        _from: NodeId,
        _pkg: &PackageAnnounce,
        _dest: &Path,
    ) -> anyhow::Result<PathBuf> {
        anyhow::bail!("fetch_manifest not supported by RetryProbeTransport")
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();
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

// ---------------------------------------------------------------------------
// Item 3: an actively-pulled batch defers the ack-timeout ladder (serve ticks
// push the deadline out) and a resumed tick disarms a retry armed mid-pull.
// ---------------------------------------------------------------------------

/// A scripted sender-side transport for the serve-activity test: it ACCEPTS every
/// announce/serve, NEVER delivers an ack, and hands the worker an event channel
/// whose sender the test keeps — so the test injects synthetic
/// [`ServeProgress`](TransportEvent::ServeProgress) ticks and the ack-timeout
/// ladder is driven purely by the per-package deadline (no real receiver). It
/// records the announce count so the test can assert the mid-pull re-announce
/// never fired, and captures the wire `package_id` minted on the first announce so
/// the test can address its ticks at the live slot.
struct ServeTickTransport {
    node_id: NodeId,
    /// The pre-made event receiver handed to the worker on `events()`. The paired
    /// sender is retained by the test; no ack is ever sent through it.
    rx: std::sync::Mutex<Option<mpsc::Receiver<TransportEvent>>>,
    /// Number of `announce()` calls — 1 means no mid-pull re-announce ran.
    announce_count: Arc<AtomicUsize>,
    /// The `package_id` minted on the first announce (== the slot's announce id the
    /// engine correlates `ServeProgress` against).
    announced_pid: Arc<std::sync::Mutex<Option<PackageId>>>,
}

#[async_trait::async_trait]
impl SharingTransport for ServeTickTransport {
    async fn start(&self) -> anyhow::Result<StartInfo> {
        Ok(StartInfo {
            node_id: self.node_id,
            pairing_ticket: String::new(),
        })
    }
    async fn announce(
        &self,
        _to: NodeId,
        a: &PackageAnnounce,
        _batch_name: &str,
        _batch_uuid: &str,
        _files: &[AnnounceFileEntry],
    ) -> anyhow::Result<()> {
        self.announce_count.fetch_add(1, SeqCst);
        *self.announced_pid.lock().unwrap() = Some(a.package_id.clone());
        Ok(())
    }
    async fn fetch(
        &self,
        _from: NodeId,
        _pkg: &PackageAnnounce,
        _dest: &Path,
        _sink: FetchSink,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn fetch_manifest(
        &self,
        _from: NodeId,
        _pkg: &PackageAnnounce,
        _dest: &Path,
    ) -> anyhow::Result<PathBuf> {
        anyhow::bail!("fetch_manifest not supported by ServeTickTransport")
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
        _pid: &PackageId,
        _r: Vec<FrameReceipt>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn release(&self, _pid: &PackageId) -> anyhow::Result<()> {
        Ok(())
    }
    async fn events(&self) -> mpsc::Receiver<TransportEvent> {
        self.rx.lock().unwrap().take().expect("events() called once")
    }
}

/// Item 3: while a peer is actively pulling, serve ticks push the ack-timeout
/// deadline out — so the "waiting · retry in N min" ladder can NEVER fire mid-pull
/// (no `next_retry_at`, no `ack_timeout` journal, no re-announce). Three phases:
/// (1) steady ticks suppress the timeout entirely; (2) silence arms the ladder,
/// then a resumed tick disarms it (countdown cleared, no re-announce); (3) full
/// silence lets the ladder resume from the last-activity instant.
#[tokio::test]
async fn serve_activity_defers_ack_timeout_and_cancels_armed_retry() {
    let tmp = tempdir().unwrap();
    let peer: NodeId = *iroh::SecretKey::generate().public().as_bytes();

    // The worker will consume `rx`; the test keeps `tx` to inject synthetic serve
    // ticks and never sends an ack (the ladder is deadline-driven).
    let (tx, rx) = mpsc::channel::<TransportEvent>(64);
    let announce_count = Arc::new(AtomicUsize::new(0));
    let announced_pid = Arc::new(std::sync::Mutex::new(None::<PackageId>));
    let transport = Arc::new(ServeTickTransport {
        node_id: peer,
        rx: std::sync::Mutex::new(Some(rx)),
        announce_count: Arc::clone(&announce_count),
        announced_pid: Arc::clone(&announced_pid),
    });

    // Short-but-generous ack timeout (review fix: 200ms flaked under parallel-test
    // scheduler stalls — a >200ms gap between processed ticks let the timer fire).
    // 600ms against 100ms ticks gives a 6× margin; every wait below scales to this
    // base (backoff rung 1 = 2× base = 1.2s).
    let ack_timeout = Duration::from_millis(600);
    let pkg = build_package(&tmp.path().join("src-serve"), "uuid-serve", "f.fits", "M8", 1024);
    let db_path = tmp.path().join("sync.db");
    let store = Arc::new(StandaloneSyncStore::open(&db_path).unwrap());
    let engine = SyncEngine::spawn_inner(
        store.clone() as Arc<dyn SyncStore>,
        transport as Arc<dyn SharingTransport>,
        peer,
        SyncConfig { ack_timeout },
        None,
        None,
        None,
    );

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();

    // The first announce mints the wire package_id + arms the ack-wait deadline.
    wait_until(|| announced_pid.lock().unwrap().is_some(), WAIT).await;
    let pid = announced_pid.lock().unwrap().clone().unwrap();
    assert_eq!(announce_count.load(SeqCst), 1, "exactly one announce so far");

    // ---- Phase 1: steady ticks suppress the ack timeout entirely. -------------
    // Tick every ~100ms (6× under the 600ms ack timeout) for > 3× the timeout,
    // ALTERNATING ServeProgress and ServeFileProgress — BOTH handlers must defer
    // (review fix: the per-file call site was previously untested). Each tick
    // pushes the deadline out, so the ladder can never fire and the row never
    // enters the "waiting · retry" presentation (next_retry_at is the persisted
    // countdown behind that UI state).
    for i in 0..20u64 {
        if i % 2 == 0 {
            tx.send(TransportEvent::ServeProgress {
                package_id: pid.clone(),
                bytes_sent: (i + 1) * 64,
            })
            .await
            .unwrap();
        } else {
            tx.send(TransportEvent::ServeFileProgress {
                package_id: pid.clone(),
                file: "f.fits".to_string(),
                bytes_done: (i + 1) * 64,
                bytes_total: 1024,
            })
            .await
            .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            store.get_outbound(id).unwrap().unwrap().next_retry_at,
            None,
            "an actively-pulled batch must never arm a retry countdown (tick {i})"
        );
    }
    let kinds = sent_event_kinds(&db_path, id);
    assert!(
        !kinds.contains(&"ack_timeout".to_string()),
        "steady serve ticks must suppress the ack timeout, got {kinds:?}"
    );
    assert_eq!(
        announce_count.load(SeqCst),
        1,
        "no mid-pull re-announce while the peer is actively pulling"
    );

    // ---- Phase 2: silence arms the ladder; a resumed tick disarms it. ---------
    // Stop ticking → the last-pushed AwaitAck deadline (200ms) elapses → the ack
    // timeout fires: next_retry_at is set and a Retry is armed (rung 1, 400ms).
    wait_until(
        || store.get_outbound(id).unwrap().unwrap().next_retry_at.is_some(),
        WAIT,
    )
    .await;
    let kinds = sent_event_kinds(&db_path, id);
    assert!(
        kinds.contains(&"ack_timeout".to_string()),
        "silence must let one ack timeout fire, got {kinds:?}"
    );
    // The Retry deadline is 400ms out; the arm itself did not re-announce yet.
    assert_eq!(
        announce_count.load(SeqCst),
        1,
        "arming the ladder does not itself re-announce"
    );

    // Resume ticks BEFORE the 400ms backoff elapses: the first tick flips the armed
    // Retry back to AwaitAck and clears the persisted countdown.
    let mut cleared = false;
    for _ in 0..50 {
        tx.send(TransportEvent::ServeProgress {
            package_id: pid.clone(),
            bytes_sent: 4096,
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        if store.get_outbound(id).unwrap().unwrap().next_retry_at.is_none() {
            cleared = true;
            break;
        }
    }
    assert!(cleared, "a resumed serve tick must cancel the armed retry countdown");
    // The disarmed Retry never fired — the flip pre-empted the re-announce.
    assert_eq!(
        announce_count.load(SeqCst),
        1,
        "the disarmed retry must not re-announce mid-pull"
    );

    // ---- Phase 2b: a ServeFileProgress tick ALSO disarms an armed retry. ------
    // (Review fix: pins the on_serve_file_progress call site independently.)
    wait_until(
        || store.get_outbound(id).unwrap().unwrap().next_retry_at.is_some(),
        WAIT,
    )
    .await;
    let mut cleared = false;
    for _ in 0..50 {
        tx.send(TransportEvent::ServeFileProgress {
            package_id: pid.clone(),
            file: "f.fits".to_string(),
            bytes_done: 512,
            bytes_total: 1024,
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        if store.get_outbound(id).unwrap().unwrap().next_retry_at.is_none() {
            cleared = true;
            break;
        }
    }
    assert!(cleared, "a per-file serve tick must cancel the armed retry countdown");
    assert_eq!(announce_count.load(SeqCst), 1, "still no mid-pull re-announce");

    // ---- Phase 2c: ServeComplete ALSO defers/disarms (the post-upload window). -
    // (Review fix: pins the on_serve_complete call site — the peer finished
    // pulling; the ack window restarts from HERE, and an armed retry is disarmed.)
    wait_until(
        || store.get_outbound(id).unwrap().unwrap().next_retry_at.is_some(),
        WAIT,
    )
    .await;
    tx.send(TransportEvent::ServeComplete { package_id: pid.clone() })
        .await
        .unwrap();
    wait_until(
        || store.get_outbound(id).unwrap().unwrap().next_retry_at.is_none(),
        WAIT,
    )
    .await;
    assert_eq!(announce_count.load(SeqCst), 1, "serve-complete must not re-announce");

    // ---- Phase 3: full silence resumes the ladder. ----------------------------
    // With no more ticks, the last-pushed deadline elapses and the ack timeout
    // fires again — the ladder climbs from the last-activity instant.
    wait_until(
        || store.get_outbound(id).unwrap().unwrap().next_retry_at.is_some(),
        WAIT,
    )
    .await;
    // And the armed Retry eventually re-announces (the FULL ladder resumed once the
    // peer went quiet, not just the countdown).
    wait_until(|| announce_count.load(SeqCst) >= 2, WAIT).await;

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// Task 3.6: a classified dial failure + a fresh re-resolve REPLACES (not merges)
// the peer's stale address-lookup entry, so a dead relay URL can't survive.
// ---------------------------------------------------------------------------

/// A probe transport whose announce always FAILS with a `timeout`-classified
/// error (so the engine records a dead-addressing class on the pending package),
/// and whose peer-address writes go through a REAL
/// [`MemoryLookup`](iroh::address_lookup::memory::MemoryLookup) exactly as the
/// production node does: `add_peer_addr` MERGES via `add_endpoint_info`,
/// `remove_peer_addr` clears via `remove_endpoint_info`. This lets the test assert
/// the lookup's final addressing after the retry path runs.
struct ReplaceProbeTransport {
    node_id: NodeId,
    lookup: iroh::address_lookup::memory::MemoryLookup,
    /// Retained so the engine's `events()` receiver never closes → no ack ever
    /// arrives → the retry path keeps running.
    keep_tx: std::sync::Mutex<Option<mpsc::Sender<TransportEvent>>>,
    /// How many times the retry path invalidated the stale lookup entry.
    removed: Arc<std::sync::Mutex<u32>>,
}

#[async_trait::async_trait]
impl SharingTransport for ReplaceProbeTransport {
    async fn start(&self) -> anyhow::Result<StartInfo> {
        Ok(StartInfo {
            node_id: self.node_id,
            pairing_ticket: String::new(),
        })
    }
    async fn announce(
        &self,
        _to: NodeId,
        _a: &PackageAnnounce,
        _batch_name: &str,
        _batch_uuid: &str,
        _files: &[AnnounceFileEntry],
    ) -> anyhow::Result<()> {
        // `classify_send_error` maps "timed out" → `ConnectClass::Timeout`, a
        // dead-addressing class that gates the Task 3.6 replace.
        anyhow::bail!("connection timed out")
    }
    async fn fetch(
        &self,
        _from: NodeId,
        _pkg: &PackageAnnounce,
        _dest: &Path,
        _sink: FetchSink,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn fetch_manifest(
        &self,
        _from: NodeId,
        _pkg: &PackageAnnounce,
        _dest: &Path,
    ) -> anyhow::Result<PathBuf> {
        anyhow::bail!("fetch_manifest not supported by ReplaceProbeTransport")
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
        _pid: &PackageId,
        _r: Vec<FrameReceipt>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn release(&self, _pid: &PackageId) -> anyhow::Result<()> {
        Ok(())
    }
    fn add_peer_addr(&self, addr: iroh::EndpointAddr) {
        // Mirror the production node's `add_peer`: MERGE into the lookup.
        self.lookup.add_endpoint_info(addr);
    }
    fn remove_peer_addr(&self, peer: NodeId) {
        // Mirror the production node's `remove_peer_addr`.
        if let Ok(id) = iroh::EndpointId::from_bytes(&peer) {
            self.lookup.remove_endpoint_info(id);
        }
        *self.removed.lock().unwrap() += 1;
    }
    async fn events(&self) -> mpsc::Receiver<TransportEvent> {
        let (tx, rx) = mpsc::channel(8);
        *self.keep_tx.lock().unwrap() = Some(tx);
        rx
    }
}

/// Task 3.6: with a STALE relay URL cached in the lookup, a dial failure classified
/// as a dead-addressing class followed by a fresh re-resolve must leave the lookup
/// holding ONLY the fresh addressing — the dead relay URL (which a merge could never
/// drop) is removed first, so re-adding the fresh address is a full replace.
#[tokio::test]
async fn retry_replaces_stale_addressing_after_classified_dial_failure() {
    let tmp = tempdir().unwrap();
    let peer: NodeId = *iroh::SecretKey::generate().public().as_bytes();
    let peer_id = iroh::EndpointId::from_bytes(&peer).unwrap();

    let stale_relay = "https://stale.relay.example./";
    let fresh_relay = "https://fresh.relay.example./";

    // A shared MemoryLookup pre-seeded with the peer's STALE relay URL — the dead
    // path a merge could never remove.
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    lookup.add_endpoint_info(
        iroh::EndpointAddr::new(peer_id).with_relay_url(stale_relay.parse().unwrap()),
    );

    let removed = Arc::new(std::sync::Mutex::new(0u32));
    let transport = Arc::new(ReplaceProbeTransport {
        node_id: peer,
        lookup: lookup.clone(),
        keep_tx: std::sync::Mutex::new(None),
        removed: Arc::clone(&removed),
    });

    // The refresher yields the peer's CURRENT (fresh) address — a different relay.
    let refresher: crate::sync::engine::AddrRefresher = Arc::new(move |p: NodeId| {
        Box::pin(async move {
            let id = iroh::EndpointId::from_bytes(&p).ok()?;
            Some(iroh::EndpointAddr::new(id).with_relay_url(fresh_relay.parse().unwrap()))
        })
    });

    let pkg = build_package(&tmp.path().join("src-t36"), "uuid-t36", "f.fits", "M8", 1024);
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
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

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();

    // The announce fails (timeout class) and arms a retry; when the backoff window
    // elapses, the retry path re-resolves, REMOVES the stale entry, then re-adds the
    // fresh one. Wait on the fresh relay appearing in the lookup.
    wait_until(
        || {
            lookup
                .get_endpoint_info(peer_id)
                .map(|info| info.relay_urls().any(|u| u.to_string() == fresh_relay))
                .unwrap_or(false)
        },
        WAIT,
    )
    .await;

    // Delivery is retried forever — a network condition never terminalizes.
    assert!(
        !matches!(
            state_of(&store, id),
            Some(OutboundState::Failed | OutboundState::Confirmed)
        ),
        "a retrying package must stay non-terminal, got {:?}",
        state_of(&store, id)
    );

    // The lookup now holds ONLY the fresh addressing — the dead relay URL is gone.
    let info = lookup
        .get_endpoint_info(peer_id)
        .expect("peer must remain in the lookup after the replace");
    let relays: Vec<String> = info.relay_urls().map(|u| u.to_string()).collect();
    assert!(
        relays.iter().any(|u| u == fresh_relay),
        "the fresh relay must be present after the replace, got {relays:?}"
    );
    assert!(
        !relays.iter().any(|u| u == stale_relay),
        "the stale relay URL must be removed (full replace, not merge), got {relays:?}"
    );
    assert!(
        *removed.lock().unwrap() >= 1,
        "the retry path must invalidate the stale lookup entry before re-adding"
    );

    engine.shutdown().await;
}

/// Task 3.4: when the retry path's address refresher is wired but yields `None`
/// (hub blip / peer gone), the engine keeps last-known addressing AND logs a
/// `warn` naming the peer — the fallback is no longer silent.
#[tokio::test]
async fn retry_refresh_unavailable_warns_with_peer() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let captured: Arc<std::sync::Mutex<Vec<CapturedEvent>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let _guard = tracing_subscriber::registry()
        .with(CaptureLayer {
            events: captured.clone(),
        })
        .set_default();

    let tmp = tempdir().unwrap();
    let peer: NodeId = *iroh::SecretKey::generate().public().as_bytes();
    // Announce succeeds → the ack never comes → the ack deadline fires → the Retry
    // arm runs, where the refresher (wired, always `None`) triggers the warn.
    let transport = Arc::new(RetryProbeTransport {
        node_id: peer,
        keep_tx: std::sync::Mutex::new(None),
        added: Arc::new(std::sync::Mutex::new(Vec::new())),
    });
    let refresher: crate::sync::engine::AddrRefresher =
        Arc::new(|_p: NodeId| Box::pin(async { None::<iroh::EndpointAddr> }));

    let pkg = build_package(&tmp.path().join("src-t34"), "uuid-t34", "f.fits", "M8", 1024);
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn_inner(
        store.clone() as Arc<dyn SyncStore>,
        transport as Arc<dyn SharingTransport>,
        peer,
        SyncConfig {
            ack_timeout: Duration::from_millis(50),
        },
        None,
        None,
        Some(refresher),
    );
    let _id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();

    const MSG: &str = "retry: address refresh unavailable; dialing last-known address";
    wait_until(
        || captured.lock().unwrap().iter().any(|e| e.message == MSG),
        WAIT,
    )
    .await;

    let events = captured.lock().unwrap();
    let warn = events
        .iter()
        .find(|e| e.message == MSG)
        .expect("expected the refresh-unavailable warn line");
    assert!(
        warn.fields.contains_key("peer"),
        "the warn must carry a peer field, got {:?}",
        warn.fields
    );
    drop(events);

    engine.shutdown().await;
}

// ── Task 5: kick / send-now (immediate out-of-band retry, backoff reset) ──────

/// Drive an engine into a long backoff window against an OFFLINE peer, returning
/// the store, the engine handle, the receiver endpoint (still offline), and the
/// row id. The peer's mailbox is never registered while offline, so every
/// announce fails ("peer not started") and nothing buffers — once the peer comes
/// online the ONLY way it sees an announce is a fresh re-attempt, which is
/// exactly what a kick triggers. This isolates the kick as the delivering event
/// (no coincidental buffered announce / natural retry can confirm first).
async fn park_in_long_backoff(
    tmp: &tempfile::TempDir,
    net: &LoopbackNetwork,
    label: &str,
) -> (
    Arc<StandaloneSyncStore>,
    SyncEngineHandle,
    Arc<LoopbackTransport>,
    i64,
) {
    // Peer endpoint minted but NOT started → announces fail, engine backs off.
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.node_id();

    let pkg = build_package(
        &tmp.path().join(format!("src_{label}")),
        &format!("uuid-{label}"),
        &format!("frame_{label}.fits"),
        "M101",
        2048,
    );

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join(format!("sync_{label}.db"))).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig {
            // 400 ms base, not 100: an unstarted peer classifies as `not_started`,
            // which D1 puts on the FLAT absent schedule (`base * 4`) instead of the
            // escalating rungs. The window these tests need — one long enough that a
            // fast confirmation can only be the kick's doing — is therefore set by
            // the base, and 400 ms * 4 = 1.6 s clears the >1.2 s bar below. The
            // point of the test is unchanged; only what produces the long window is.
            ack_timeout: Duration::from_millis(400),
        },
    );

    let id = engine.enqueue_package(&pkg, None, Vec::new()).await.unwrap();

    // A couple of failed announce attempts against the absent peer (spec §2).
    wait_until(|| attempts_of(&store, id) >= 2, WAIT).await;

    // Wait until the engine is parked in a LONG backoff window: next_retry_at
    // parses to comfortably (>1.2s) in the future. Without a kick the next natural
    // re-announce is that far off, so a fast confirmation below can only be the
    // kick's doing.
    wait_until(
        || {
            store
                .get_outbound(id)
                .unwrap()
                .and_then(|r| r.next_retry_at)
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                .map(|t| t.with_timezone(&chrono::Utc) > chrono::Utc::now() + chrono::Duration::milliseconds(1200))
                .unwrap_or(false)
        },
        WAIT,
    )
    .await;
    assert_ne!(
        state_of(&store, id),
        Some(OutboundState::Confirmed),
        "the package must not have delivered while the peer was offline"
    );

    (store, engine, receiver, id)
}

/// Step 1 (the named acceptance test): a package sitting out a long backoff
/// window against a now-online peer delivers **immediately** when kicked. The
/// kick collapses the retry deadline to now and resets the rung, so the next
/// worker pass re-announces instead of waiting out the >1.2s window — proving
/// the deadline collapsed.
// multi_thread runtime: the 800ms confirmation bound is structurally tight on
// current_thread, which starves the single OS thread under host load (audit
// TEST-8). Two workers keep the timing bound honest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kick_fires_immediate_attempt_and_resets_backoff() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let (store, engine, receiver, id) = park_in_long_backoff(&tmp, &net, "kick").await;

    // Peer comes online (mailbox empty — no buffered announces) and starts
    // acking. WITHOUT the kick the package would sit out its >1.2s backoff.
    receiver.start().await.unwrap();
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv_kick"));

    // Kick one package: collapse the deadline to now + reset the rung.
    engine.kick(id).await.unwrap();

    // Confirmation lands FAR sooner than the pending >1.2s backoff window would
    // have allowed (8×ack_timeout is generous for CI yet well under the natural
    // retry) — this only happens because the kick collapsed the deadline.
    wait_until(
        || state_of(&store, id) == Some(OutboundState::Confirmed),
        Duration::from_millis(800),
    )
    .await;

    let row = store.get_outbound(id).unwrap().unwrap();
    assert_eq!(row.state, OutboundState::Confirmed);
    assert!(row.confirmed_at.is_some(), "confirmed_at must be stamped");
    assert_eq!(
        row.next_retry_at, None,
        "a confirmed package clears the persisted retry countdown"
    );

    engine.shutdown().await;
}

/// `kick_all` applies the same immediate-retry reset to every pending slot, and
/// the app-side [`SyncSenderRuntime::kick_all`] fans it out over every started
/// engine (fire-and-forget). Driven through the runtime holder to lock that
/// contract end to end: an engine parked in a long backoff delivers immediately
/// once the runtime kicks it.
// multi_thread runtime: same tight timing bound as the other kick tests; a
// current_thread runtime starves under host load (audit TEST-8).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_kick_all_wakes_every_started_engine() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let (store, engine, receiver, id) = park_in_long_backoff(&tmp, &net, "kickall").await;
    let peer = receiver.node_id();

    // Peer online, acking; empty mailbox (offline announces all failed).
    receiver.start().await.unwrap();
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv_kickall"));

    // Register the running engine in a fresh sender runtime, then kick every
    // started engine through it.
    let runtime = SyncSenderRuntime::new();
    runtime.lock_inner().await.insert(
        peer,
        StartedSender {
            engine: Arc::new(engine),
            origin_device: "origin-device".to_string(),
            peer,
        },
    );
    runtime.kick_all().await;

    // The runtime's fire-and-forget kick collapses the engine's backoff, so the
    // package delivers well within the >1.2s window it was parked in.
    wait_until(
        || state_of(&store, id) == Some(OutboundState::Confirmed),
        Duration::from_millis(800),
    )
    .await;
    assert_eq!(state_of(&store, id), Some(OutboundState::Confirmed));

    // Drain the runtime and shut the engine down cleanly.
    let started = runtime.lock_inner().await.remove(&peer).unwrap();
    started.engine.shutdown().await;
}

/// Task 6 (the named handle-level acceptance test): `kick_all` resets EVERY
/// pending slot, not just one. Two packages are parked mid-backoff against an
/// offline peer (empty mailbox — every offline announce failed, so nothing
/// buffered); the peer then comes online and a single `engine.kick_all()`
/// collapses BOTH backoff windows, so both confirm promptly (well inside the
/// >1.2s window they were parked in). This is the engine-level surface of the
/// node's wake hook: a relay reconnect / relay-map change fires the hook, which
/// fans `SyncSenderRuntime::kick_all` — itself `engine.kick_all()` — over every
/// engine, and every pending package must wake, not merely the first.
// multi_thread runtime: same tight timing bound as the other kick tests; a
// current_thread runtime starves under host load (audit TEST-8).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kick_all_resets_every_pending_package() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    // Peer endpoint minted but NOT started → every announce fails, engine backs
    // off. Its mailbox stays empty, so once it comes online the only way it sees
    // an announce is a fresh kick-driven re-attempt (no buffered announce can
    // confirm first).
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.node_id();

    let pkg_a = build_package(
        &tmp.path().join("src_a"),
        "uuid-multi-a",
        "frame_a.fits",
        "M101",
        2048,
    );
    let pkg_b = build_package(
        &tmp.path().join("src_b"),
        "uuid-multi-b",
        "frame_b.fits",
        "M101",
        4096,
    );

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync_multi.db")).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig {
            // 400 ms for the same reason as `park_in_long_backoff`: an unstarted
            // peer is the FLAT absent schedule (`base * 4`), so the base is what
            // sets the >1.2 s window this test needs.
            ack_timeout: Duration::from_millis(400),
        },
    );

    let id_a = engine.enqueue_package(&pkg_a, None, Vec::new()).await.unwrap();
    let id_b = engine.enqueue_package(&pkg_b, None, Vec::new()).await.unwrap();

    // Park both against the absent peer. Under D1 coalescing only the HEAD (the
    // lowest pending id) keeps a live countdown; its queue-mate parks with no
    // deadline at all and waits for a signal. Either way, neither re-announces on
    // its own inside the window below, so a fast confirmation can only be the
    // kick's doing.
    let countdown_beyond_1200ms = |id: i64| {
        store
            .get_outbound(id)
            .unwrap()
            .and_then(|r| r.next_retry_at)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|t| {
                t.with_timezone(&chrono::Utc)
                    > chrono::Utc::now() + chrono::Duration::milliseconds(1200)
            })
            .unwrap_or(false)
    };
    let parked_without_countdown = |id: i64| {
        store
            .get_outbound(id)
            .unwrap()
            .map(|r| r.next_retry_at.is_none())
            .unwrap_or(false)
    };
    wait_until(|| attempts_of(&store, id_a) >= 1 && attempts_of(&store, id_b) >= 1, WAIT).await;
    wait_until(
        || countdown_beyond_1200ms(id_a) && parked_without_countdown(id_b),
        WAIT,
    )
    .await;
    assert_ne!(
        state_of(&store, id_a),
        Some(OutboundState::Confirmed),
        "package a must not deliver while the peer is offline"
    );
    assert_ne!(
        state_of(&store, id_b),
        Some(OutboundState::Confirmed),
        "package b must not deliver while the peer is offline"
    );

    // Reach: a single kick_all must collapse BOTH deadlines — including the parked
    // queue-mate, which has no deadline of its own and is released by nothing else
    // while the peer stays absent. Asserted BEFORE the peer comes online, so the
    // proof cannot be confused with the head's success releasing its queue.
    let attempts_before = (attempts_of(&store, id_a), attempts_of(&store, id_b));
    engine.kick_all().await.unwrap();
    wait_until(
        || {
            attempts_of(&store, id_a) > attempts_before.0
                && attempts_of(&store, id_b) > attempts_before.1
        },
        Duration::from_millis(800),
    )
    .await;

    // Peer online (mailbox empty), acking. A second kick_all delivers both.
    receiver.start().await.unwrap();
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv_multi"));
    engine.kick_all().await.unwrap();

    // BOTH confirm far sooner than the pending >1.2s windows would have allowed —
    // proof that kick_all collapsed both deadlines, not just one.
    wait_until(
        || {
            state_of(&store, id_a) == Some(OutboundState::Confirmed)
                && state_of(&store, id_b) == Some(OutboundState::Confirmed)
        },
        Duration::from_millis(900),
    )
    .await;

    for id in [id_a, id_b] {
        let row = store.get_outbound(id).unwrap().unwrap();
        assert_eq!(row.state, OutboundState::Confirmed, "id {id} must confirm");
        assert_eq!(
            row.next_retry_at, None,
            "a confirmed package clears the persisted retry countdown"
        );
    }

    engine.shutdown().await;
}

/// Kicking an id with no pending slot (already terminal / unknown) is a silent
/// no-op — the handle call succeeds and nothing changes.
#[tokio::test]
async fn kick_unknown_id_is_a_no_op() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;

    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
    );

    // No package enqueued: id 999 has no pending slot. Kick must not error.
    engine.kick(999).await.unwrap();
    // kick_all over an empty pending map is likewise a no-op.
    engine.kick_all().await.unwrap();

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// Transfers Status Model v2 (tv2 Task 4): per-file states, persisted `uploaded`
// (Delivered), error ⊥ state, event journal.
// ---------------------------------------------------------------------------

/// Per-payload manifest entries for `enqueue_package`, from `(uuid, name, size)`.
fn entries(frames: &[(&str, &str, u64)]) -> Vec<AnnounceFileEntry> {
    frames
        .iter()
        .map(|(uuid, name, size)| AnnounceFileEntry {
            rel_path: name.to_string(),
            byte_size: *size,
            frame_uuid: uuid.to_string(),
        })
        .collect()
}

/// The outbound per-file rows for `id` via the `SyncStore` trait method.
fn files_of(store: &StandaloneSyncStore, id: i64) -> Vec<crate::sync::OutboundFileRow> {
    store.list_outbound_files(id).unwrap()
}

/// The one file row whose `rel_path == rel` (panics if absent).
fn file_row<'a>(
    rows: &'a [crate::sync::OutboundFileRow],
    rel: &str,
) -> &'a crate::sync::OutboundFileRow {
    rows.iter()
        .find(|r| r.rel_path == rel)
        .unwrap_or_else(|| panic!("no file row {rel} in {rows:?}"))
}

/// The `sent`-direction `sync_events` kinds for `id`, read via a fresh connection
/// to the WAL db (committed events are visible to a second reader).
fn sent_event_kinds(db_path: &Path, id: i64) -> Vec<String> {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    crate::sync::store::list_sync_events(&conn, Direction::Sent, &id.to_string())
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

/// A reactive receiver that fetches on every announce but acks (Ingested) only
/// while `allow_ack` is true — lets a test hold a transfer in flight (no ack)
/// across a timeout/restart, then let it confirm.
fn spawn_gated_receiver(
    endpoint: Arc<LoopbackTransport>,
    dest_root: PathBuf,
    allow_ack: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let mut events = endpoint.events().await;
        let mut n = 0usize;
        while let Some(event) = events.recv().await {
            let TransportEvent::AnnounceReceived { from, announce, .. } = event else {
                continue;
            };
            n += 1;
            let dest = dest_root.join(format!("fetch-{n}"));
            if endpoint.fetch(from, &announce, &dest, noop_fetch_sink()).await.is_ok() {
                if !allow_ack.load(SeqCst) {
                    continue;
                }
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
                let _ = endpoint.ack(from, &announce.package_id, receipts).await;
            }
        }
    });
}

/// A reactive receiver that fetches then acks every frame as `Rejected(reason)` —
/// drives the sender's per-file rejected settlement while the batch stays in flight.
fn spawn_rejecting_receiver(endpoint: Arc<LoopbackTransport>, dest_root: PathBuf, reason: &str) {
    let reason = reason.to_string();
    tokio::spawn(async move {
        let mut events = endpoint.events().await;
        let mut n = 0usize;
        while let Some(event) = events.recv().await {
            let TransportEvent::AnnounceReceived { from, announce, .. } = event else {
                continue;
            };
            n += 1;
            let dest = dest_root.join(format!("fetch-{n}"));
            if endpoint.fetch(from, &announce, &dest, noop_fetch_sink()).await.is_ok() {
                let Ok(records) = package::read_manifest(&dest) else {
                    continue;
                };
                let receipts: Vec<FrameReceipt> = records
                    .iter()
                    .map(|r| FrameReceipt {
                        frame_uuid: r.frame_uuid.clone(),
                        xxh3: r.xxh3.clone(),
                        outcome: ReceiptOutcome::Rejected(reason.clone()),
                    })
                    .collect();
                let _ = endpoint.ack(from, &announce.package_id, receipts).await;
            }
        }
    });
}

/// tv2 §D1/§D4: `enqueue_package` writes the outbound row, its `display_name`, AND
/// its `pending` per-file rows in ONE transaction — so the whole picture exists the
/// instant the call returns, before the worker's first announce (the T3
/// enqueue→announce race is gone). The `enqueued` journal event lands too.
#[tokio::test]
async fn enqueue_writes_row_files_and_name_before_first_announce() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();
    // A peer that is never started → the worker's first announce always fails, so
    // the file rows never leave `pending`.
    let peer = [42u8; 32];

    let pkg = build_package_multi(
        &tmp.path().join("src"),
        "meta",
        &[("uuid-a", "a.fits", "M42", 2048), ("uuid-b", "b.fits", "M42", 4096)],
    );
    let db_path = tmp.path().join("sync.db");
    let store = Arc::new(StandaloneSyncStore::open(&db_path).unwrap());
    let engine = SyncEngine::spawn(store.clone() as Arc<dyn SyncStore>, Arc::new(net.endpoint()), peer);

    let files = entries(&[("uuid-a", "a.fits", 2048), ("uuid-b", "b.fits", 4096)]);
    let id = engine
        .enqueue_package(&pkg, Some("M42 — 2 lights".to_string()), files)
        .await
        .unwrap();

    // Atomically after the call returns: row name + both `pending` file rows exist.
    let row = store.get_outbound(id).unwrap().unwrap();
    assert_eq!(row.display_name.as_deref(), Some("M42 — 2 lights"));
    let rows = files_of(&store, id);
    assert_eq!(rows.len(), 2, "both file rows present: {rows:?}");
    for r in &rows {
        assert_eq!(r.state, OutboundFileState::Pending);
        assert_eq!(r.bytes_done, 0);
        assert!(r.outcome.is_none());
    }
    assert_eq!(file_row(&rows, "a.fits").byte_size, 2048);
    assert_eq!(file_row(&rows, "b.fits").frame_uuid, "uuid-b");
    assert!(
        sent_event_kinds(&db_path, id).contains(&"enqueued".to_string()),
        "the enqueued journal event landed"
    );

    engine.shutdown().await;
}

/// tv2 §D4: a want-subset send settles the EXCLUDED file terminal immediately
/// (`done`/`duplicate` — the peer already has it) and the WANTED file walks to
/// `done`/`ingested` across a full loopback transfer.
#[tokio::test]
async fn want_subset_settles_excluded_duplicate_and_wanted_ingested() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();
    // The peer already has "a.fits"; it wants only "b.fits".
    let receiver = Arc::new(net.endpoint_with_responder(Arc::new(WantOnlyResponder {
        wanted_rel: "b.fits".to_string(),
    })));
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let pkg = build_package_multi(
        &tmp.path().join("srcWant"),
        "want",
        &[("uuid-a", "a.fits", "M42", 2048), ("uuid-b", "b.fits", "M42", 4096)],
    );
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(store.clone() as Arc<dyn SyncStore>, Arc::new(net.endpoint()), receiver_id);
    let files = entries(&[("uuid-a", "a.fits", 2048), ("uuid-b", "b.fits", 4096)]);
    let id = engine.enqueue_package(&pkg, None, files).await.unwrap();

    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;

    let rows = files_of(&store, id);
    let a = file_row(&rows, "a.fits");
    assert_eq!(a.state, OutboundFileState::Done, "excluded file is terminal");
    assert_eq!(a.outcome.as_deref(), Some("duplicate"), "excluded file is a duplicate");
    let b = file_row(&rows, "b.fits");
    assert_eq!(b.state, OutboundFileState::Done, "wanted file is terminal");
    assert_eq!(b.outcome.as_deref(), Some("ingested"), "wanted file was ingested");

    engine.shutdown().await;
}

/// tv2 §D4/§D5: `ServeComplete` persists the batch `uploaded` stage as the reserved
/// non-terminal `Delivered` state (every file row `uploaded`) and survives restart —
/// a fresh engine over the same store re-drives the `Delivered` row exactly like a
/// `Transferring` one and confirms on the (now-allowed) ack.
#[tokio::test]
async fn serve_complete_persists_delivered_uploaded_then_restart_confirms() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let allow_ack = Arc::new(AtomicBool::new(false));
    spawn_gated_receiver(receiver.clone(), tmp.path().join("recv"), allow_ack.clone());

    let pkg = build_package_multi(
        &tmp.path().join("srcDel"),
        "del",
        &[("uuid-a", "a.fits", "M42", 2048), ("uuid-b", "b.fits", "M42", 4096)],
    );
    let db_path = tmp.path().join("sync.db");
    let store_a = Arc::new(StandaloneSyncStore::open(&db_path).unwrap());
    // Default (long) ack timeout: the row rests in Delivered awaiting the ack.
    let engine_a = SyncEngine::spawn(store_a.clone() as Arc<dyn SyncStore>, Arc::new(net.endpoint()), receiver_id);
    let files = entries(&[("uuid-a", "a.fits", 2048), ("uuid-b", "b.fits", 4096)]);
    let id = engine_a.enqueue_package(&pkg, None, files).await.unwrap();

    // Phase 1: the peer pulled everything (ServeComplete) but did not ack → the
    // batch persists `Delivered` and every file row is `uploaded`.
    wait_until(|| state_of(&store_a, id) == Some(OutboundState::Delivered), WAIT).await;
    let rows = files_of(&store_a, id);
    assert_eq!(rows.len(), 2);
    for r in &rows {
        assert_eq!(r.state, OutboundFileState::Uploaded, "uploaded, not yet acked: {r:?}");
    }
    assert!(sent_event_kinds(&db_path, id).contains(&"uploaded".to_string()));

    // Phase 2: a fresh engine over the SAME store re-drives the Delivered row; now
    // that acks are allowed it confirms and every file row lands `done`/`ingested`.
    drop(engine_a);
    allow_ack.store(true, SeqCst);
    let store_b = Arc::new(StandaloneSyncStore::open(&db_path).unwrap());
    let engine_b = SyncEngine::spawn(store_b.clone() as Arc<dyn SyncStore>, Arc::new(net.endpoint()), receiver_id);
    wait_until(|| state_of(&store_b, id) == Some(OutboundState::Confirmed), WAIT).await;
    let rows = files_of(&store_b, id);
    for r in &rows {
        assert_eq!(r.state, OutboundFileState::Done);
        assert_eq!(r.outcome.as_deref(), Some("ingested"), "file {}: {r:?}", r.rel_path);
    }

    engine_b.shutdown().await;
}

/// tv2 §D5/§D7: an ack timeout sets `last_error` and journals `ack_timeout` +
/// `retry_scheduled`; the subsequent successful delivery confirms, clears the stale
/// error, and journals `ack_received` + `confirmed`.
#[tokio::test]
async fn ack_timeout_journals_retry_then_recovery_confirms_and_clears_error() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let allow_ack = Arc::new(AtomicBool::new(false));
    spawn_gated_receiver(receiver.clone(), tmp.path().join("recv"), allow_ack.clone());

    let pkg = build_package(&tmp.path().join("srcTo"), "uuid-to", "f.fits", "M42", 4096);
    let db_path = tmp.path().join("sync.db");
    let store = Arc::new(StandaloneSyncStore::open(&db_path).unwrap());
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig { ack_timeout: Duration::from_millis(300) },
    );
    let id = engine
        .enqueue_package(&pkg, None, entries(&[("uuid-to", "f.fits", 4096)]))
        .await
        .unwrap();

    // Phase 1: the peer pulls but never acks → the ack times out. `last_error` set,
    // journal records ack_timeout + retry_scheduled.
    wait_until(|| store.get_outbound(id).unwrap().unwrap().last_error.is_some(), WAIT).await;
    assert_eq!(
        store.get_outbound(id).unwrap().unwrap().last_error.as_deref(),
        Some("no ack from peer within timeout")
    );
    let kinds = sent_event_kinds(&db_path, id);
    assert!(kinds.contains(&"ack_timeout".to_string()), "kinds: {kinds:?}");
    assert!(kinds.contains(&"retry_scheduled".to_string()), "kinds: {kinds:?}");

    // Phase 2: allow the ack → a re-announce confirms; no stale error survives,
    // journal records ack_received + confirmed.
    allow_ack.store(true, SeqCst);
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;
    let row = store.get_outbound(id).unwrap().unwrap();
    assert_eq!(row.last_error, None, "a confirmed transfer carries no stale reason");
    let kinds = sent_event_kinds(&db_path, id);
    assert!(kinds.contains(&"ack_received".to_string()), "kinds: {kinds:?}");
    assert!(kinds.contains(&"confirmed".to_string()), "kinds: {kinds:?}");

    engine.shutdown().await;
}

/// tv2 §D4: a `Rejected` receipt settles the per-file row `done` with a
/// `rejected:<reason>` outcome AND the per-file error, while the batch itself stays
/// in flight (a rejection redelivers forever, never terminalizes).
#[tokio::test]
async fn rejected_receipt_settles_file_with_rejected_outcome() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    spawn_rejecting_receiver(receiver.clone(), tmp.path().join("recv"), "bad hash");

    let pkg = build_package(&tmp.path().join("srcRej"), "uuid-r", "f.fits", "M42", 4096);
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    // Long ack timeout so no retry re-serves during the window (which would flip the
    // rejected row back through `sending`/`uploaded`).
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig { ack_timeout: Duration::from_secs(60) },
    );
    let id = engine
        .enqueue_package(&pkg, None, entries(&[("uuid-r", "f.fits", 4096)]))
        .await
        .unwrap();

    wait_until(
        || {
            files_of(&store, id)
                .first()
                .and_then(|r| r.outcome.clone())
                .map(|o| o.starts_with("rejected:"))
                .unwrap_or(false)
        },
        WAIT,
    )
    .await;
    let rows = files_of(&store, id);
    let f = file_row(&rows, "f.fits");
    assert_eq!(f.state, OutboundFileState::Done);
    assert_eq!(f.outcome.as_deref(), Some("rejected:bad hash"));
    assert_eq!(f.error.as_deref(), Some("bad hash"));
    // The batch is NOT confirmed — a rejection redelivers, never terminalizes.
    assert_ne!(state_of(&store, id), Some(OutboundState::Confirmed));

    engine.shutdown().await;
}

/// tv2 §D4: a fetch aborted AFTER a file's start-tick but BEFORE its completion
/// leaves the per-file row `sending` (the intermediate state); the resumed serve
/// then uploads it and the batch confirms `done`/`ingested`.
#[tokio::test]
async fn mid_transfer_abort_leaves_file_sending_then_resume_uploads() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    receiver.set_fault(FaultPlan {
        abort_after_bytes: Some(64),
        ..Default::default()
    });
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let pkg = build_package(&tmp.path().join("srcSend"), "uuid-s", "f.fits", "M42", 4096);
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    // Long ack timeout so the aborted transfer rests `sending` until we kick it.
    let engine = SyncEngine::spawn_with_config(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
        SyncConfig { ack_timeout: Duration::from_secs(60) },
    );
    let id = engine
        .enqueue_package(&pkg, None, entries(&[("uuid-s", "f.fits", 4096)]))
        .await
        .unwrap();

    // The receiver's first fetch aborts after the file's start-tick (sending) but
    // before its completion → the per-file row rests `sending`.
    wait_until(
        || files_of(&store, id).first().map(|r| r.state) == Some(OutboundFileState::Sending),
        WAIT,
    )
    .await;
    assert_eq!(files_of(&store, id)[0].state, OutboundFileState::Sending);

    // Kick the retry: the second fetch completes → the file uploads → batch confirms.
    engine.kick(id).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;
    let rows = files_of(&store, id);
    assert_eq!(rows[0].state, OutboundFileState::Done);
    assert_eq!(rows[0].outcome.as_deref(), Some("ingested"));

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// Transfers Batch Model (B3): resend-as-reset, per-attempt wire ids, Revoke.
// ---------------------------------------------------------------------------

/// Announce file entries derived from a package's on-disk manifest — the §D4
/// per-file rows a real send carries into `enqueue_package`.
fn announce_files_of(pkg: &Path) -> Vec<AnnounceFileEntry> {
    package::read_manifest(pkg)
        .unwrap()
        .into_iter()
        .map(|r| AnnounceFileEntry {
            rel_path: r.rel_path,
            byte_size: r.byte_size,
            frame_uuid: r.frame_uuid,
        })
        .collect()
}

/// `Ingested` receipts for every frame in a package's manifest.
fn ingested_receipts_of(pkg: &Path) -> Vec<FrameReceipt> {
    package::read_manifest(pkg)
        .unwrap()
        .into_iter()
        .map(|r| FrameReceipt {
            frame_uuid: r.frame_uuid,
            xxh3: r.xxh3,
            outcome: ReceiptOutcome::Ingested,
        })
        .collect()
}

/// The persisted per-attempt wire id of outbound row `id`.
fn wire_of(store: &StandaloneSyncStore, id: i64) -> Option<String> {
    store.get_outbound(id).unwrap().and_then(|r| r.wire_package_id)
}

/// Whether `id` has announced + is serving/awaiting-ack — `Transferring` OR the
/// post-upload `Delivered` (a receiver that fetches advances it to the latter).
/// The single non-terminal "announce reached the peer" signal the resend/revoke
/// tests wait on.
fn is_serving(store: &StandaloneSyncStore, id: i64) -> bool {
    matches!(
        state_of(store, id),
        Some(OutboundState::Transferring) | Some(OutboundState::Delivered)
    )
}

/// Shared observation state for the resend/revoke tests: every announce id seen,
/// every revoke `(package_id, reason)` seen, and a switch to withhold acks.
#[derive(Clone)]
struct Recorder {
    announces: Arc<std::sync::Mutex<Vec<String>>>,
    revokes: Arc<std::sync::Mutex<Vec<(String, RevokeReason)>>>,
    ack_enabled: Arc<AtomicBool>,
}

impl Recorder {
    fn new(ack_enabled: bool) -> Self {
        Self {
            announces: Arc::new(std::sync::Mutex::new(Vec::new())),
            revokes: Arc::new(std::sync::Mutex::new(Vec::new())),
            ack_enabled: Arc::new(AtomicBool::new(ack_enabled)),
        }
    }
    fn announces(&self) -> Vec<String> {
        self.announces.lock().unwrap().clone()
    }
    fn revokes(&self) -> Vec<(String, RevokeReason)> {
        self.revokes.lock().unwrap().clone()
    }
}

/// A reactive receiver that records every announce id + revoke and acks `Ingested`
/// on each announce ONLY while `ack_enabled` (so a test can hold a transfer in
/// `awaiting-ack` and cancel it, then let the resend confirm).
fn spawn_recording_receiver(endpoint: Arc<LoopbackTransport>, dest_root: PathBuf, rec: Recorder) {
    tokio::spawn(async move {
        let mut events = endpoint.events().await;
        let mut n = 0usize;
        while let Some(event) = events.recv().await {
            match event {
                TransportEvent::AnnounceReceived { from, announce, .. } => {
                    rec.announces.lock().unwrap().push(announce.package_id.0.clone());
                    n += 1;
                    let dest = dest_root.join(format!("fetch-{n}"));
                    if endpoint.fetch(from, &announce, &dest, noop_fetch_sink()).await.is_ok()
                        && rec.ack_enabled.load(SeqCst)
                    {
                        if let Ok(records) = package::read_manifest(&dest) {
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
                    }
                }
                TransportEvent::RevokeReceived { package_id, reason, .. } => {
                    rec.revokes.lock().unwrap().push((package_id.0, reason));
                }
                _ => {}
            }
        }
    });
}

/// The full one-row resend cycle over loopback (Transfers Batch Model §D1):
/// enqueue → cancel mid-serve → resend → confirm, ALL on ONE row. Pins that the
/// row id is constant, the wire id rotates per attempt (and the resend announces
/// the NEW one — the stale-wire-id regression), the cancel emits
/// `Revoke{Cancelled}` for the old wire id, per-file rows reset then settle, and
/// the journal reads cancelled → revoke_sent → resend → confirmed in order.
#[tokio::test]
async fn resend_cycle_one_row_cancel_then_confirm() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    // Acks withheld: attempt 1 parks in awaiting-ack so the cancel lands mid-serve.
    let rec = Recorder::new(false);
    spawn_recording_receiver(receiver.clone(), tmp.path().join("recv"), rec.clone());

    let pkg = build_package(&tmp.path().join("src"), "uuid-rc", "rc.fits", "M42", 4096);
    let files = announce_files_of(&pkg);
    let db_path = tmp.path().join("sync.db");
    let store = Arc::new(StandaloneSyncStore::open(&db_path).unwrap());
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
    );

    let id = engine
        .enqueue_package(&pkg, Some("RC batch".into()), files)
        .await
        .unwrap();

    // Attempt 1 announces (acks withheld) → row parks Transferring; capture w1.
    wait_until(|| is_serving(&store, id), WAIT).await;
    let w1 = wire_of(&store, id).expect("attempt 1 persisted a wire id");

    // Cancel mid-serve → Cancelled + Revoke{Cancelled}(w1).
    engine.cancel(id).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Cancelled), WAIT).await;
    wait_until(
        || rec.revokes().iter().any(|(pid, r)| *pid == w1 && *r == RevokeReason::Cancelled),
        WAIT,
    )
    .await;

    // Resend: reset the SAME row (fresh wire id) + journal `resend` (as the api
    // layer does), enable acks, then re-drive.
    let w2 = uuid::Uuid::new_v4().to_string();
    store.reset_outbound_for_resend(id, &w2).unwrap();
    store
        .append_sync_event(Direction::Sent, &id.to_string(), "resend", None)
        .unwrap();
    rec.ack_enabled.store(true, SeqCst);
    engine.resend(id).await.unwrap();

    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;

    // ONE row: same id, attempts advanced, wire rotated to the resend's id.
    let row = store.get_outbound(id).unwrap().unwrap();
    assert!(row.attempts >= 2, "attempts advanced past the original, got {}", row.attempts);
    assert_ne!(w2, w1, "resend rotated the wire id");
    assert_eq!(
        row.wire_package_id.as_deref(),
        Some(w2.as_str()),
        "the confirmed attempt kept the resend's wire id",
    );

    // The resend announced the NEW wire id (no stale caching across the reset).
    let announces = rec.announces();
    assert!(announces.contains(&w1), "attempt 1 announced w1, got {announces:?}");
    assert!(announces.contains(&w2), "the resend announced the NEW wire id w2, got {announces:?}");

    // Per-file rows reset then settled to done/ingested on the resend.
    let rows = files_of(&store, id);
    assert!(!rows.is_empty(), "the batch has per-file rows");
    assert!(
        rows.iter().all(|r| r.state == OutboundFileState::Done && r.outcome.as_deref() == Some("ingested")),
        "every file settled ingested on the resend, got {rows:?}",
    );

    // Journal order: cancelled → revoke_sent → resend → confirmed. `sent_event_kinds`
    // is newest-first; reverse to chronological so `position` reads left-to-right.
    let mut kinds = sent_event_kinds(&db_path, id);
    kinds.reverse();
    let pos = |k: &str| kinds.iter().position(|x| x == k);
    assert!(pos("cancelled").is_some(), "journal has cancelled, got {kinds:?}");
    assert!(pos("revoke_sent").is_some(), "journal has revoke_sent, got {kinds:?}");
    assert!(pos("resend").is_some(), "journal has resend, got {kinds:?}");
    assert!(pos("confirmed").is_some(), "journal has confirmed, got {kinds:?}");
    assert!(pos("cancelled") < pos("revoke_sent"), "cancelled before revoke_sent, got {kinds:?}");
    assert!(pos("revoke_sent") < pos("resend"), "revoke_sent before resend, got {kinds:?}");
    assert!(pos("resend") < pos("confirmed"), "resend before confirmed, got {kinds:?}");

    engine.shutdown().await;
}

/// Attempt-isolation (§D1, item 5): a resend mints a fresh wire id, so an ack
/// carrying the OLD attempt's wire id can NEVER short-circuit the new attempt.
/// Seeds a stale `Ingested` ack keyed by the cancelled attempt's wire id and
/// asserts the row does NOT confirm; only an ack on the fresh wire id does.
#[tokio::test]
async fn old_attempt_wire_id_ack_does_not_replay_onto_resend() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    // Never auto-acks — this test drives the acks by hand.
    let rec = Recorder::new(false);
    spawn_recording_receiver(receiver.clone(), tmp.path().join("recv"), rec.clone());

    let pkg = build_package(&tmp.path().join("src"), "uuid-replay", "r.fits", "M42", 4096);
    let files = announce_files_of(&pkg);
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    // Keep the sender endpoint so we know its node id for the hand-driven acks.
    let sender_ep = Arc::new(net.endpoint());
    let sender_node = sender_ep.node_id();
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        sender_ep as Arc<dyn SharingTransport>,
        receiver_id,
    );

    let id = engine.enqueue_package(&pkg, None, files).await.unwrap();
    wait_until(|| is_serving(&store, id), WAIT).await;
    let w1 = wire_of(&store, id).expect("attempt 1 wire id");

    // Cancel → Cancelled, then resend with a fresh wire id.
    engine.cancel(id).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Cancelled), WAIT).await;
    let w2 = uuid::Uuid::new_v4().to_string();
    store.reset_outbound_for_resend(id, &w2).unwrap();
    engine.resend(id).await.unwrap();
    // Wait until the resend has re-announced the NEW wire id (slot now holds w2 and
    // is awaiting the ack — the fetch may have already advanced it to Delivered).
    wait_until(|| rec.announces().contains(&w2), WAIT).await;
    wait_until(|| is_serving(&store, id), WAIT).await;

    // STALE ack keyed by the cancelled attempt's wire id: must be ignored.
    receiver
        .ack(sender_node, &PackageId(w1.clone()), ingested_receipts_of(&pkg))
        .await
        .unwrap();
    // Give the sender a moment to (not) act on it.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_ne!(
        state_of(&store, id),
        Some(OutboundState::Confirmed),
        "a stale old-wire-id ack must NOT confirm the resent attempt",
    );

    // FRESH ack on the resend's wire id: confirms.
    receiver
        .ack(sender_node, &PackageId(w2.clone()), ingested_receipts_of(&pkg))
        .await
        .unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Confirmed), WAIT).await;
    assert_eq!(wire_of(&store, id).as_deref(), Some(w2.as_str()));

    engine.shutdown().await;
}

/// A [`DedupResponder`] whose verdict flips on a shared flag: want-all while
/// `false` (session 1 gets a real announce), all-duplicate while `true` (session
/// 2's re-negotiate finds the peer already holds every frame).
struct SwitchableResponder {
    all_duplicate: Arc<AtomicBool>,
}
impl DedupResponder for SwitchableResponder {
    fn want_for_offer(&self, entries: &[OfferEntry]) -> (Vec<String>, Vec<String>) {
        if self.all_duplicate.load(SeqCst) {
            (Vec::new(), entries.iter().map(|e| e.rel_path.clone()).collect())
        } else {
            (entries.iter().map(|e| e.rel_path.clone()).collect(), Vec::new())
        }
    }
    fn confirm_full_hashes(&self, _entries: &[FullHashEntry]) -> Vec<String> {
        Vec::new()
    }
}

/// The superseded race (§F3/§D2): a `Transferring` row whose announce already
/// reached the peer is re-driven (crash-resume) and this time the dedup handshake
/// finds the peer holds every frame → all-duplicate confirm. Because a prior
/// announce for this SAME wire id left the receiver a row, the sender must emit
/// `Revoke{Superseded}` for it. Constructed via the traced path: session 1
/// announces (want-all), the engine is killed mid-serve, the peer flips to
/// all-duplicate, session 2's crash-resume re-negotiates and terminalizes.
#[tokio::test]
async fn revoke_superseded_on_all_duplicate_after_prior_announce() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let all_dup = Arc::new(AtomicBool::new(false));
    let receiver = Arc::new(net.endpoint_with_responder(Arc::new(SwitchableResponder {
        all_duplicate: all_dup.clone(),
    })));
    let receiver_id = receiver.start().await.unwrap().node_id;
    let rec = Recorder::new(false); // no acks: session 1 must stay Transferring.
    spawn_recording_receiver(receiver.clone(), tmp.path().join("recv"), rec.clone());

    let pkg = build_package(&tmp.path().join("src"), "uuid-sup", "s.fits", "M42", 4096);
    let files = announce_files_of(&pkg);
    let db_path = tmp.path().join("sync.db");
    let store1 = Arc::new(StandaloneSyncStore::open(&db_path).unwrap());
    let engine1 = SyncEngine::spawn(
        store1.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
    );

    // Session 1: want-all → serve + announce → Transferring. Capture the wire id.
    let id = engine1.enqueue_package(&pkg, None, files).await.unwrap();
    wait_until(|| is_serving(&store1, id), WAIT).await;
    let w1 = wire_of(&store1, id).expect("attempt 1 wire id");

    // Peer now holds everything; kill session 1 mid-serve (row stays Transferring).
    all_dup.store(true, SeqCst);
    engine1.shutdown().await;

    // Session 2: fresh engine, SAME store → crash-resume re-negotiates → the peer
    // reports all-duplicate → terminalize as confirmed AND revoke the stuck row.
    let store2 = Arc::new(StandaloneSyncStore::open(&db_path).unwrap());
    let engine2 = SyncEngine::spawn(
        store2.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
    );

    wait_until(|| state_of(&store2, id) == Some(OutboundState::Confirmed), WAIT).await;
    wait_until(
        || rec.revokes().iter().any(|(pid, r)| *pid == w1 && *r == RevokeReason::Superseded),
        WAIT,
    )
    .await;

    // The journal records the superseded revoke.
    let kinds = sent_event_kinds(&db_path, id);
    assert!(kinds.iter().any(|k| k == "revoke_sent"), "journal has revoke_sent, got {kinds:?}");

    engine2.shutdown().await;
}

/// Local terminal failure (§D2): when the package payload vanishes after its
/// announce reached the peer, the re-drive fails the transfer terminally
/// (`Failed`) and must best-effort `Revoke{Failed}` the outstanding announce so
/// the receiver tears down.
#[tokio::test]
async fn revoke_failed_on_local_terminal_failure() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let rec = Recorder::new(false); // no acks → stays Transferring after announce.
    spawn_recording_receiver(receiver.clone(), tmp.path().join("recv"), rec.clone());

    let pkg = build_package(&tmp.path().join("src"), "uuid-fail", "f.fits", "M42", 4096);
    let files = announce_files_of(&pkg);
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_id,
    );

    let id = engine.enqueue_package(&pkg, None, files).await.unwrap();
    wait_until(|| is_serving(&store, id), WAIT).await;
    let w1 = wire_of(&store, id).expect("attempt 1 wire id");

    // Delete the payload dir, then kick a re-attempt: `attempt` sees the dir gone
    // and fails the package terminally (the ONE local-unrecoverable case).
    std::fs::remove_dir_all(&pkg).unwrap();
    engine.kick(id).await.unwrap();

    wait_until(|| state_of(&store, id) == Some(OutboundState::Failed), WAIT).await;
    wait_until(
        || rec.revokes().iter().any(|(pid, r)| *pid == w1 && *r == RevokeReason::Failed),
        WAIT,
    )
    .await;

    engine.shutdown().await;
}

/// A transport whose `revoke` blocks for a long time (a dead peer's delivery-ack
/// never comes) but delegates everything else to a working loopback endpoint.
struct SlowRevokeTransport(Arc<LoopbackTransport>);

#[async_trait::async_trait]
impl SharingTransport for SlowRevokeTransport {
    async fn start(&self) -> anyhow::Result<StartInfo> {
        self.0.start().await
    }
    async fn announce(
        &self,
        to: NodeId,
        a: &PackageAnnounce,
        batch_name: &str,
        batch_uuid: &str,
        files: &[AnnounceFileEntry],
    ) -> anyhow::Result<()> {
        self.0.announce(to, a, batch_name, batch_uuid, files).await
    }
    async fn revoke(
        &self,
        _to: NodeId,
        _package_id: &PackageId,
        _reason: RevokeReason,
    ) -> anyhow::Result<()> {
        // A dead peer: the delivery-acked control never settles. If the cancel
        // path awaited this, it would stall ~30s — the test proves it does not.
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(())
    }
    async fn fetch(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
        sink: FetchSink,
    ) -> anyhow::Result<()> {
        self.0.fetch(from, pkg, dest_dir, sink).await
    }
    async fn fetch_manifest(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
    ) -> anyhow::Result<PathBuf> {
        self.0.fetch_manifest(from, pkg, dest_dir).await
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
    async fn release(&self, package_id: &PackageId) -> anyhow::Result<()> {
        self.0.release(package_id).await
    }
    async fn events(&self) -> mpsc::Receiver<TransportEvent> {
        self.0.events().await
    }
}

/// Cancel latency is independent of the revoke's delivery-ack (B1 seam): the
/// revoke rides a delivery-acked control that can hang ~30s on a dead peer, but
/// `cancel_package` spawns it fire-and-forget, so the row reaches `Cancelled`
/// immediately. Uses a transport whose `revoke` sleeps 30s.
#[tokio::test]
async fn cancel_with_dead_peer_revoke_does_not_stall() {
    let tmp = tempdir().unwrap();
    let net = LoopbackNetwork::new();

    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let rec = Recorder::new(false); // no acks → stays Transferring after announce.
    spawn_recording_receiver(receiver.clone(), tmp.path().join("recv"), rec.clone());

    let pkg = build_package(&tmp.path().join("src"), "uuid-dead", "d.fits", "M42", 4096);
    let files = announce_files_of(&pkg);
    let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
    // The sender's transport delegates announce/serve to a working endpoint but its
    // revoke sleeps 30s (dead peer).
    let sender = Arc::new(SlowRevokeTransport(Arc::new(net.endpoint())));
    let engine = SyncEngine::spawn(
        store.clone() as Arc<dyn SyncStore>,
        sender,
        receiver_id,
    );

    let id = engine.enqueue_package(&pkg, None, files).await.unwrap();
    wait_until(|| is_serving(&store, id), WAIT).await;

    // Cancel and time how long the terminal transition takes: it must NOT wait on
    // the 30s revoke.
    let started = Instant::now();
    engine.cancel(id).await.unwrap();
    wait_until(|| state_of(&store, id) == Some(OutboundState::Cancelled), WAIT).await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "cancel must not stall on the slow revoke (took {elapsed:?})",
    );

    engine.shutdown().await;
}
