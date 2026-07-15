//! End-to-end acceptance test for the Perseus agent over the in-process
//! loopback transport (task A6, brief Step 1).
//!
//! Covers:
//! - Two fixture FITS dropped into a watched capture dir are each enqueued
//!   exactly once (write-stability window respected via a short config),
//!   packaged with header-derived `frame_meta`, and driven to `Confirmed`
//!   against an in-test receiver stub that fetches + acks.
//! - Unclean shutdown mid-transfer (hard-kill the agent's background tasks
//!   while a row rests in `Transferring`) followed by restart over the same
//!   `data_dir` resumes and completes.
//! - Store-aware dedup (review IMPORTANT #2): a frame written while the agent
//!   is NOT running is not lost on restart (the money test); an unchanged
//!   already-sent file is never re-enqueued across a restart; a genuinely
//!   modified file (same path, different content) IS re-enqueued.
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
use athenaeum_core::sharing::{noop_fetch_sink, SharingTransport};
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
        capture_dir: Some(capture_dir.to_path_buf()),
        capture_dirs: Vec::new(),
        data_dir: data_dir.to_path_buf(),
        // Loopback path never parses this as an iroh ticket; any non-empty value
        // passes structural validation. No [account] table — the loopback e2e
        // exercises the dev-ticket path (task M1).
        pairing_ticket: Some("loopback-test".to_string()),
        account: None,
        targets: Vec::new(),
        device_name: None,
        mode: Mode::Auto,
        // A 1s quiet window (matching stability/poll) so the batcher flushes each
        // stabilized frame within seconds — the e2e asserts confirmation inside
        // `WAIT`, and the production 60s default would blow that budget.
        auto_quiet_secs: 1,
        retention: RetentionConfig {
            policy: RetentionPolicy::KeepEverything,
            dry_run: true,
            ..RetentionConfig::default()
        },
        stability_secs: 1,
        poll_interval_secs: 1,
        // The web status page is disabled in the loopback e2e (and would never
        // bind anyway — these agents use `start_with_transport`, which never
        // spawns the server).
        web_bind: String::new(),
        web_token: None,
    }
}

/// A multi-directory variant of [`test_config`]: `capture_dirs = [...]` with no
/// singular `capture_dir`. Used by the task-7 smoke test to watch two dirs.
fn test_config_multi(capture_dirs: &[&Path], data_dir: &Path) -> Config {
    Config {
        capture_dir: None,
        capture_dirs: capture_dirs.iter().map(|p| p.to_path_buf()).collect(),
        data_dir: data_dir.to_path_buf(),
        pairing_ticket: Some("loopback-test".to_string()),
        account: None,
        targets: Vec::new(),
        device_name: None,
        mode: Mode::Auto,
        // Short quiet window so the batcher flushes promptly (see `test_config`).
        auto_quiet_secs: 1,
        retention: RetentionConfig {
            policy: RetentionPolicy::KeepEverything,
            dry_run: true,
            ..RetentionConfig::default()
        },
        stability_secs: 1,
        poll_interval_secs: 1,
        // The web status page is disabled in the loopback e2e (and would never
        // bind anyway — these agents use `start_with_transport`, which never
        // spawns the server).
        web_bind: String::new(),
        web_token: None,
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
            direction: None,
            peer: None,
            project: None,
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
            direction: None,
            peer: None,
            project: None,
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

    // Unclean shutdown: hard-kill agent A's background tasks (watcher, enqueue
    // consumer) and drop its engine handle, rather than a bare `drop(agent_a)`.
    // A bare drop only *detaches* those tokio tasks — they keep running, and
    // since the injected fault is one-shot (disarms after the first failure),
    // the zombie agent A could quietly finish the transfer itself on its own
    // retry, making this test pass for the wrong reason instead of proving
    // agent B's crash-resume is what completes it. The durable row stays
    // Transferring in <data_dir>/perseus.db either way.
    agent_a.kill_for_test();

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

// ---------------------------------------------------------------------------
// Store-aware dedup (review IMPORTANT #2): perseus_seen must close the
// restart-window gap without either losing a frame or re-sending forever.
// ---------------------------------------------------------------------------

/// The money test: a frame written while the agent is NOT running must not be
/// silently lost. The pre-fix baseline marked every file present at startup as
/// already-handled unconditionally, so this file would never be enqueued; the
/// fix (`perseus_seen`-backed dedup) treats an unrecorded file as a genuine new
/// arrival regardless of when it appeared.
#[tokio::test]
async fn file_written_while_agent_down_is_enqueued_after_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let capture = tmp.path().join("capture");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&capture).unwrap();

    let net = LoopbackNetwork::new();
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    // The agent has never run: this frame lands in the capture dir "during
    // downtime", before Perseus starts for the first time.
    write_fixture_fits(&capture.join("during_downtime.fits"), "M1");

    let sender = net.endpoint();
    let sender_id = sender.node_id();
    let transport: Arc<dyn SharingTransport> = Arc::new(sender);
    let cfg = test_config(&capture, &data);
    let agent = Agent::start_with_transport(cfg, transport, receiver_id, sender_id, true)
        .await
        .expect("start agent");

    wait_until(
        || is_confirmed(&history_for(&agent, "during_downtime.fits")),
        WAIT,
    )
    .await;
    assert_eq!(
        sent_starts(&history_for(&agent, "during_downtime.fits")),
        1,
        "the downtime frame must be enqueued exactly once, not lost"
    );

    agent.shutdown().await;
}

/// A file already confirmed in a prior run, left untouched on disk, must NOT
/// be re-packaged and re-sent just because the agent restarted.
#[tokio::test]
async fn confirmed_unchanged_file_is_not_reenqueued_after_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let capture = tmp.path().join("capture");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&capture).unwrap();

    let net = LoopbackNetwork::new();
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    // Agent A: enqueues + confirms the frame, then shuts down cleanly.
    let sender_a = net.endpoint();
    let sender_a_id = sender_a.node_id();
    let transport_a: Arc<dyn SharingTransport> = Arc::new(sender_a);
    let cfg_a = test_config(&capture, &data);
    let agent_a = Agent::start_with_transport(cfg_a, transport_a, receiver_id, sender_a_id, true)
        .await
        .expect("start agent A");

    tokio::time::sleep(Duration::from_millis(500)).await;
    write_fixture_fits(&capture.join("frame.fits"), "M51");
    wait_until(|| is_confirmed(&history_for(&agent_a, "frame.fits")), WAIT).await;
    assert_eq!(sent_starts(&history_for(&agent_a, "frame.fits")), 1);
    agent_a.shutdown().await;

    // Agent B: same data_dir/capture_dir, file untouched on disk since A wrote it.
    let sender_b = net.endpoint();
    let sender_b_id = sender_b.node_id();
    let transport_b: Arc<dyn SharingTransport> = Arc::new(sender_b);
    let cfg_b = test_config(&capture, &data);
    let agent_b = Agent::start_with_transport(cfg_b, transport_b, receiver_id, sender_b_id, true)
        .await
        .expect("start agent B");

    // Give the watcher several stability+poll cycles to (not) rediscover it.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        sent_starts(&history_for(&agent_b, "frame.fits")),
        1,
        "an unchanged, already-confirmed file must not be re-sent after restart"
    );

    agent_b.shutdown().await;
}

/// A file genuinely rewritten between runs (same path, different content/size,
/// so a different `(size, mtime)`) must be re-enqueued — the stat drift proves
/// it isn't the same frame anymore.
#[tokio::test]
async fn modified_file_is_reenqueued_after_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let capture = tmp.path().join("capture");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&capture).unwrap();
    let path = capture.join("frame.fits");

    let net = LoopbackNetwork::new();
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    // Agent A: enqueues + confirms the frame, then shuts down cleanly.
    let sender_a = net.endpoint();
    let sender_a_id = sender_a.node_id();
    let transport_a: Arc<dyn SharingTransport> = Arc::new(sender_a);
    let cfg_a = test_config(&capture, &data);
    let agent_a = Agent::start_with_transport(cfg_a, transport_a, receiver_id, sender_a_id, true)
        .await
        .expect("start agent A");

    tokio::time::sleep(Duration::from_millis(500)).await;
    write_fixture_fits(&path, "M81");
    wait_until(|| is_confirmed(&history_for(&agent_a, "frame.fits")), WAIT).await;
    assert_eq!(sent_starts(&history_for(&agent_a, "frame.fits")), 1);
    agent_a.shutdown().await;

    // Rewrite the file in place: different dimensions guarantee a different
    // byte size regardless of filesystem mtime resolution.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let cards = HeaderBuilder::new(FrameKind::Light)
        .object("M81")
        .exptime(120.0)
        .filter("Ha")
        .instrume("TestCam")
        .build()
        .expect("build header");
    let data_px = vec![0.0f32; 16 * 16];
    write_fits_f32(&path, 16, 16, 1, &data_px, &cards).expect("rewrite fixture fits");

    // Agent B: same data_dir/capture_dir; the changed stat must re-enqueue.
    let sender_b = net.endpoint();
    let sender_b_id = sender_b.node_id();
    let transport_b: Arc<dyn SharingTransport> = Arc::new(sender_b);
    let cfg_b = test_config(&capture, &data);
    let agent_b = Agent::start_with_transport(cfg_b, transport_b, receiver_id, sender_b_id, true)
        .await
        .expect("start agent B");

    wait_until(
        || sent_starts(&history_for(&agent_b, "frame.fits")) >= 2,
        WAIT,
    )
    .await;

    agent_b.shutdown().await;
}

// ---------------------------------------------------------------------------
// Task 7: multiple capture directories. A config with `capture_dirs = [a, b]`
// arms one watcher per directory, all feeding the single enqueue pipeline — a
// file dropped in EACH directory is packaged and confirmed independently.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_capture_dirs_are_both_watched() {
    let tmp = tempfile::tempdir().unwrap();
    let cap_a = tmp.path().join("capture-a");
    let cap_b = tmp.path().join("capture-b");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&cap_a).unwrap();
    std::fs::create_dir_all(&cap_b).unwrap();

    let net = LoopbackNetwork::new();
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _stats = spawn_receiver(receiver.clone(), tmp.path().join("recv"));

    let sender = net.endpoint();
    let sender_id = sender.node_id();
    let transport: Arc<dyn SharingTransport> = Arc::new(sender);

    let cfg = test_config_multi(&[cap_a.as_path(), cap_b.as_path()], &data);
    let agent = Agent::start_with_transport(cfg, transport, receiver_id, sender_id, true)
        .await
        .expect("start agent");

    // Let both watchers finish their (empty) baseline scan before the files land,
    // so each fixture is a NEW arrival rather than pre-existing baseline.
    tokio::time::sleep(Duration::from_millis(500)).await;
    write_fixture_fits(&cap_a.join("frame_a.fits"), "M42");
    write_fixture_fits(&cap_b.join("frame_b.fits"), "M31");

    // Both files — one from each watched directory — reach Confirmed.
    wait_until(
        || {
            is_confirmed(&history_for(&agent, "frame_a.fits"))
                && is_confirmed(&history_for(&agent, "frame_b.fits"))
        },
        WAIT,
    )
    .await;

    // Each was enqueued exactly once → exactly one transfer-start row each.
    assert_eq!(
        sent_starts(&history_for(&agent, "frame_a.fits")),
        1,
        "the file in capture dir A must be enqueued exactly once"
    );
    assert_eq!(
        sent_starts(&history_for(&agent, "frame_b.fits")),
        1,
        "the file in capture dir B must be enqueued exactly once"
    );

    // Everything terminalized: no rows left in flight across BOTH dirs.
    wait_until(|| agent.status_snapshot().unwrap().is_empty(), WAIT).await;

    agent.shutdown().await;
}
