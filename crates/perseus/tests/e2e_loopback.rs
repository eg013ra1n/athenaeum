//! End-to-end acceptance test for the Perseus agent over the in-process
//! loopback transport (task A6, brief Step 1).
//!
//! Covers:
//! - Two fixture FITS dropped into a watched capture dir are each enqueued
//!   exactly once (write-stability window respected via a short config),
//!   packaged with header-derived `frame_meta`, and driven to `Confirmed`
//!   against an in-test receiver stub that fetches + acks.
//! - Unclean shutdown mid-transfer (drop the agent while a row rests in
//!   `Transferring`) followed by restart over the same `data_dir` resumes and
//!   completes.
//!
//! Fixtures are generated in-test with core's sanctioned `fits_writer`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};

use athenaeum_core::fits_writer::keywords::{FrameKind, HeaderBuilder};
use athenaeum_core::fits_writer::write_fits_f32;
use athenaeum_core::package;
use athenaeum_core::sharing::loopback::{FaultPlan, LoopbackNetwork, LoopbackTransport};
use athenaeum_core::sharing::types::{FrameReceipt, ReceiptOutcome, TransportEvent};
use athenaeum_core::sharing::SharingTransport;
use athenaeum_core::sync::{HistoryQuery, HistoryRow, OutboundState, SyncStore};

use perseus::config::{Config, Mode, RetentionConfig, RetentionPolicy};
use perseus::run::Agent;

const WAIT: Duration = Duration::from_secs(30);

/// A valid single-frame FITS written via core's writer (atomic tmp+rename).
fn write_fixture_fits(path: &Path, object: &str) {
    let cards = HeaderBuilder::new(FrameKind::Light)
        .object(object)
        .exptime(60.0)
        .filter("Ha")
        .instrume("TestCam")
        .build()
        .expect("build header");
    let data = vec![0.0f32; 8 * 8];
    write_fits_f32(path, 8, 8, 1, &data, &cards).expect("write fixture fits");
}

/// A test config with short stability/poll windows so the e2e runs in seconds.
fn test_config(capture_dir: &Path, data_dir: &Path) -> Config {
    Config {
        capture_dir: capture_dir.to_path_buf(),
        data_dir: data_dir.to_path_buf(),
        // Loopback path never parses this as an iroh ticket; any non-empty value
        // passes structural validation.
        pairing_ticket: "loopback-test".to_string(),
        mode: Mode::Auto,
        retention: RetentionConfig {
            policy: RetentionPolicy::KeepEverything,
            dry_run: true,
        },
        stability_secs: 1,
        poll_interval_secs: 1,
    }
}

struct ReceiverStats {
    attempts: Arc<AtomicUsize>,
    failures: Arc<AtomicUsize>,
}

/// Reactive receiver: for every announce, fetch into a fresh dir and (on success)
/// ack every manifest frame as `Ingested`. An aborted fetch counts a failure.
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

async fn wait_until<F: FnMut() -> bool>(mut pred: F, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("wait_until timed out after {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn history_for(agent: &Agent, filename: &str) -> Vec<HistoryRow> {
    agent
        .store()
        .search_history(HistoryQuery {
            filename: Some(filename.to_string()),
            object: None,
            limit: 100,
        })
        .expect("search history")
}

fn is_confirmed(rows: &[HistoryRow]) -> bool {
    rows.iter()
        .any(|h| h.finished_at.is_some() && h.outcome == "ingested")
}

fn sent_starts(rows: &[HistoryRow]) -> usize {
    rows.iter()
        .filter(|h| h.finished_at.is_none() && h.outcome == "sent")
        .count()
}

#[tokio::test]
async fn two_fixtures_are_enqueued_once_and_confirmed() {
    let tmp = tempfile::tempdir().unwrap();
    let capture = tmp.path().join("capture");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&capture).unwrap();

    let net = LoopbackNetwork::new();
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let sender = net.endpoint();
    let sender_id = sender.node_id();
    let transport: Arc<dyn SharingTransport> = Arc::new(sender);

    let cfg = test_config(&capture, &data);
    let agent = Agent::start_with_transport(cfg, transport, receiver_id, sender_id, true)
        .await
        .expect("start agent");

    // Let the watcher's (empty) baseline scan run before the files land, so both
    // fixtures are treated as NEW arrivals rather than pre-existing baseline.
    tokio::time::sleep(Duration::from_millis(500)).await;
    write_fixture_fits(&capture.join("frame1.fits"), "M42");
    write_fixture_fits(&capture.join("frame2.fits"), "M31");

    wait_until(
        || {
            is_confirmed(&history_for(&agent, "frame1.fits"))
                && is_confirmed(&history_for(&agent, "frame2.fits"))
        },
        WAIT,
    )
    .await;

    // Each file was enqueued exactly once → exactly one transfer-start row each.
    assert_eq!(
        sent_starts(&history_for(&agent, "frame1.fits")),
        1,
        "frame1 must be enqueued exactly once"
    );
    assert_eq!(
        sent_starts(&history_for(&agent, "frame2.fits")),
        1,
        "frame2 must be enqueued exactly once"
    );

    // Header-derived frame_meta reached the manifest → object is recorded.
    let obj_rows = agent
        .store()
        .search_history(HistoryQuery {
            filename: None,
            object: Some("M42".to_string()),
            limit: 10,
        })
        .unwrap();
    assert!(
        !obj_rows.is_empty(),
        "OBJECT from the FITS header must survive into frame_meta/history"
    );

    // Everything terminalized: no rows left in flight.
    wait_until(|| agent.status_snapshot().unwrap().is_empty(), WAIT).await;

    agent.shutdown().await;
}

#[tokio::test]
async fn unclean_shutdown_mid_transfer_resumes_on_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let capture = tmp.path().join("capture");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&capture).unwrap();

    let net = LoopbackNetwork::new();
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    // Abort the receiver's first fetch partway through, leaving the sender's row
    // in Transferring with no ack.
    receiver.set_fault(FaultPlan {
        abort_after_bytes: Some(64),
        ..Default::default()
    });
    let stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    // Agent A: watches, enqueues, transfers — until we kill it.
    let sender_a = net.endpoint();
    let sender_a_id = sender_a.node_id();
    let transport_a: Arc<dyn SharingTransport> = Arc::new(sender_a);
    let cfg_a = test_config(&capture, &data);
    let agent_a = Agent::start_with_transport(cfg_a, transport_a, receiver_id, sender_a_id, true)
        .await
        .expect("start agent A");

    tokio::time::sleep(Duration::from_millis(500)).await;
    write_fixture_fits(&capture.join("frame.fits"), "NGC7000");

    // The first fetch aborts (fault fires and disarms), and the row rests in
    // Transferring.
    wait_until(|| stats.failures.load(SeqCst) >= 1, WAIT).await;
    wait_until(
        || {
            agent_a
                .status_snapshot()
                .unwrap()
                .iter()
                .any(|r| r.state == OutboundState::Transferring)
        },
        WAIT,
    )
    .await;

    // Unclean shutdown: drop the agent without awaiting shutdown. The durable
    // row stays Transferring in <data_dir>/perseus.db.
    drop(agent_a);

    // Agent B: fresh sender endpoint on the same net, same peer, same data_dir.
    // No watcher needed — crash-resume re-drives the persisted row.
    let sender_b = net.endpoint();
    let sender_b_id = sender_b.node_id();
    let transport_b: Arc<dyn SharingTransport> = Arc::new(sender_b);
    let cfg_b = test_config(&capture, &data);
    let agent_b = Agent::start_with_transport(cfg_b, transport_b, receiver_id, sender_b_id, false)
        .await
        .expect("start agent B");

    // Resume re-announces; the receiver (fault disarmed) fetches fully and acks.
    wait_until(|| is_confirmed(&history_for(&agent_b, "frame.fits")), WAIT).await;
    assert!(
        stats.attempts.load(SeqCst) >= 2,
        "resume must have triggered a second fetch"
    );
    wait_until(|| agent_b.status_snapshot().unwrap().is_empty(), WAIT).await;

    agent_b.shutdown().await;
}
