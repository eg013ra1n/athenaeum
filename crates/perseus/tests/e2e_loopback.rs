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
use athenaeum_core::sync::store::StandaloneSyncStore;
use athenaeum_core::sync::{HistoryQuery, HistoryRow, OutboundState, SyncStore};

use perseus::config::{Config, Mode, RetentionConfig, RetentionPolicy};
use perseus::run::Agent;
use perseus::supervisor::AgentState;
use perseus::web::{build_router, WebState};
use tower::ServiceExt; // ServiceExt::oneshot drives the router in-process

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
            package_id: None,
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
            package_id: None,
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

// ---------------------------------------------------------------------------
// Send-model catch-up (batch model v2.1): reset-in-place resend, confirmed
// "Send again" with payload rebuild, and the declined-transfer divert.
// ---------------------------------------------------------------------------

/// True iff `dir` still holds any non-manifest regular file (payload copies).
/// Local twin of the web layer's payload probe, for asserting cleanup states.
fn has_payload(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_type().map(|t| t.is_file()).unwrap_or(false)
            && e.file_name() != std::ffi::OsStr::new("manifest.ndjson")
    })
}

/// The standard sender-side rig for the resend e2es: loopback net, one live
/// ingesting receiver, and an agent watching `capture` with fixtures written
/// BEFORE start (they land in the initial scan → ONE batch for all of them).
async fn resend_rig(
    tmp: &Path,
    fixtures: &[&str],
    quiet_secs: u64,
) -> (Agent, Config, ReceiverStats, PathBuf) {
    let capture = tmp.join("capture");
    let data = tmp.join("data");
    std::fs::create_dir_all(&capture).unwrap();
    for name in fixtures {
        write_fixture_fits(&capture.join(name), "M42");
    }

    let net = LoopbackNetwork::new();
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let stats = spawn_receiver(receiver.clone(), tmp.join("recv"));

    let sender = net.endpoint();
    let sender_id = sender.node_id();
    let transport: Arc<dyn SharingTransport> = Arc::new(sender);

    let mut cfg = test_config(&capture, &data);
    cfg.auto_quiet_secs = quiet_secs;
    let agent = Agent::start_with_transport(cfg.clone(), transport, receiver_id, sender_id, true)
        .await
        .expect("start agent");
    (agent, cfg, stats, capture)
}

/// The "receiver deleted its copies, send everything again" cycle for a
/// CONFIRMED batch: after confirm the sender's payload copies are cleaned
/// (manifest-only dir), so the operator's "Send again" rebuilds them from the
/// original capture files, resets the SAME row (generation 2), re-delivers,
/// re-confirms — and the dir is cleaned once more. The watcher never
/// re-enqueues anything (the seen store was untouched), and the batch carries
/// its human display name end to end (T1).
#[tokio::test]
async fn confirmed_retry_rebuilds_payloads_and_reconfirms() {
    let tmp = tempfile::tempdir().unwrap();
    let (agent, cfg, stats, _capture) = resend_rig(tmp.path(), &["frame1.fits"], 1).await;

    wait_until(|| is_confirmed(&history_for(&agent, "frame1.fits")), WAIT).await;
    let rows = agent.store().all_outbound(10).unwrap();
    assert_eq!(rows.len(), 1, "one batch, one outbound row");
    let row = rows[0].clone();
    assert_eq!(
        row.display_name.as_deref(),
        Some("frame1.fits"),
        "T1: a single-file batch is named after its file"
    );
    assert_eq!(
        agent.store().list_outbound_files(row.id).unwrap().len(),
        1,
        "T1: the sender's per-file rows are written at enqueue time"
    );

    // Single-target engine: the in-line cleanup reclaims the payload copies on
    // confirm — the dir goes manifest-only, exactly the "can't retry" old shape.
    let pkg = PathBuf::from(&row.package_ref);
    wait_until(|| !has_payload(&pkg), WAIT).await;

    // "Send again": rebuild from the originals + reset the same row + re-drive.
    let batches = perseus::batch_store::BatchStore::open(cfg.db_path()).unwrap();
    let report = perseus::resend::resend_in_place(
        agent.store().as_ref(),
        &agent.engine_handle(),
        None,
        &cfg,
        &batches,
        &row,
    )
    .await
    .expect("confirmed resend accepted");
    assert!(report.rebuilt, "the manifest-only dir was rebuilt from the originals");
    assert!(report.missing.is_empty() && report.changed.is_empty());

    wait_until(
        || {
            agent
                .store()
                .get_outbound(row.id)
                .unwrap()
                .is_some_and(|r| r.state == OutboundState::Confirmed && r.generation == 2)
        },
        WAIT,
    )
    .await;
    assert_eq!(
        agent.store().all_outbound(10).unwrap().len(),
        1,
        "same row re-confirmed; the watcher re-enqueued nothing"
    );
    assert!(
        stats.attempts.load(SeqCst) >= 2,
        "the receiver fetched the batch twice (original + resend)"
    );
    // The second confirm cleans the rebuilt payload once more.
    wait_until(|| !has_payload(&pkg), WAIT).await;

    agent.shutdown().await;
}

/// "Send again" when one ORIGINAL vanished from the observatory disk: the
/// rebuild honestly reports the missing file, ships the restored subset under
/// the same row, and the manifest shrinks to what was actually re-sent.
#[tokio::test]
async fn confirmed_retry_with_deleted_original_is_partial_and_honest() {
    let tmp = tempfile::tempdir().unwrap();
    // Both fixtures pre-exist → the initial scan batches them into ONE package
    // (2s quiet window comfortably covers their same-tick stabilization).
    let (agent, cfg, _stats, capture) =
        resend_rig(tmp.path(), &["frame1.fits", "frame2.fits"], 2).await;

    wait_until(
        || {
            is_confirmed(&history_for(&agent, "frame1.fits"))
                && is_confirmed(&history_for(&agent, "frame2.fits"))
        },
        WAIT,
    )
    .await;
    let rows = agent.store().all_outbound(10).unwrap();
    assert_eq!(rows.len(), 1, "both fixtures coalesced into one batch");
    let row = rows[0].clone();
    let pkg = PathBuf::from(&row.package_ref);
    wait_until(|| !has_payload(&pkg), WAIT).await;

    // One original is gone by the time the operator hits "Send again".
    std::fs::remove_file(capture.join("frame2.fits")).unwrap();

    let batches = perseus::batch_store::BatchStore::open(cfg.db_path()).unwrap();
    let report = perseus::resend::resend_in_place(
        agent.store().as_ref(),
        &agent.engine_handle(),
        None,
        &cfg,
        &batches,
        &row,
    )
    .await
    .expect("partial resend accepted");
    assert!(report.rebuilt);
    assert_eq!(report.missing, vec!["frame2.fits"], "the lost original is NAMED");
    assert!(report.changed.is_empty());

    wait_until(
        || {
            agent
                .store()
                .get_outbound(row.id)
                .unwrap()
                .is_some_and(|r| r.state == OutboundState::Confirmed && r.generation == 2)
        },
        WAIT,
    )
    .await;
    // The batch honestly shrank to what was restorable.
    assert_eq!(
        package::read_manifest(&pkg)
            .unwrap()
            .iter()
            .map(|r| r.rel_path.clone())
            .collect::<Vec<_>>(),
        vec!["frame1.fits"],
        "the manifest was rewritten to the restored subset"
    );

    agent.shutdown().await;
}

/// Reactive receiver that DECLINES the first batch identity it sees (fetch +
/// all-`Cancelled` ack — the deliberate human "no") and ingests every other
/// batch. The divert mints a NEW batch identity, which is how its re-ask gets
/// through while any same-batch re-announce keeps bouncing.
fn spawn_decline_first_batch_receiver(
    endpoint: Arc<LoopbackTransport>,
    dest_root: PathBuf,
) -> Arc<std::sync::Mutex<Option<String>>> {
    let declined: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
    let declined_task = Arc::clone(&declined);
    tokio::spawn(async move {
        let mut events = endpoint.events().await;
        let mut n = 0usize;
        while let Some(event) = events.recv().await {
            let TransportEvent::AnnounceReceived {
                from,
                announce,
                batch_uuid,
                ..
            } = event
            else {
                continue;
            };
            n += 1;
            let decline = {
                let mut guard = declined_task.lock().unwrap();
                match guard.as_ref() {
                    None => {
                        *guard = Some(batch_uuid.clone());
                        true
                    }
                    Some(first) => *first == batch_uuid,
                }
            };
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
                        outcome: if decline {
                            ReceiptOutcome::Cancelled
                        } else {
                            ReceiptOutcome::Ingested
                        },
                    })
                    .collect();
                let _ = endpoint.ack(from, &announce.package_id, receipts).await;
            }
        }
    });
    declined
}

/// The operator divert for a receiver-declined batch: the plain retry keeps
/// bouncing off the declined guard (an autonomous agent must never override a
/// human "no"), while "Resend as new transfer" mints a NEW batch identity that
/// the receiver accepts — the old row stays Cancelled with the cross-link
/// suffix, and the seen-store linkage follows the new package.
#[tokio::test]
async fn declined_then_operator_divert_mints_new_batch_and_delivers() {
    let tmp = tempfile::tempdir().unwrap();
    let capture = tmp.path().join("capture");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&capture).unwrap();
    write_fixture_fits(&capture.join("frame1.fits"), "M42");

    let net = LoopbackNetwork::new();
    let receiver = Arc::new(net.endpoint());
    let receiver_id = receiver.start().await.unwrap().node_id;
    let _declined = spawn_decline_first_batch_receiver(receiver.clone(), tmp.path().join("recv"));

    let sender = net.endpoint();
    let sender_id = sender.node_id();
    let transport: Arc<dyn SharingTransport> = Arc::new(sender);
    let cfg = test_config(&capture, &data);
    let agent = Agent::start_with_transport(cfg.clone(), transport, receiver_id, sender_id, true)
        .await
        .expect("start agent");

    // The receiver declines the batch → Cancelled + the exact by-receiver detail.
    wait_until(
        || {
            agent.store().all_outbound(10).unwrap().first().is_some_and(|r| {
                r.state == OutboundState::Cancelled
                    && r.last_error.as_deref()
                        == Some(athenaeum_core::sync::CANCELLED_BY_RECEIVER_DETAIL)
            })
        },
        WAIT,
    )
    .await;
    let old_row = agent.store().all_outbound(10).unwrap()[0].clone();
    let old_pkg = PathBuf::from(&old_row.package_ref);

    // The in-place retry (what an autonomous or casual resend would do) bounces.
    let batches = perseus::batch_store::BatchStore::open(cfg.db_path()).unwrap();
    let bounced = perseus::resend::resend_in_place(
        agent.store().as_ref(),
        &agent.engine_handle(),
        None,
        &cfg,
        &batches,
        &old_row,
    )
    .await;
    assert!(bounced.is_err(), "a declined transfer is never retried in place");

    // The explicit operator divert mints a NEW transfer and delivers it.
    let new_id = perseus::resend::resend_declined_as_new(
        agent.store().as_ref(),
        &agent.engine_handle(),
        None,
        &cfg,
        &batches,
        &perseus::seen::SeenStore::open(cfg.db_path()).unwrap(),
        &old_row,
    )
    .await
    .expect("divert accepted");
    assert_ne!(new_id, old_row.id);

    wait_until(
        || {
            agent
                .store()
                .get_outbound(new_id)
                .unwrap()
                .is_some_and(|r| r.state == OutboundState::Confirmed)
        },
        WAIT,
    )
    .await;
    let new_row = agent.store().get_outbound(new_id).unwrap().unwrap();
    assert_ne!(
        PathBuf::from(&new_row.package_ref).file_name(),
        old_pkg.file_name(),
        "fresh dir basename ⇒ fresh wire batch identity"
    );

    // The old row is history with the cross-link; the linkage moved on.
    let old = agent.store().get_outbound(old_row.id).unwrap().unwrap();
    assert_eq!(old.state, OutboundState::Cancelled);
    assert!(old
        .last_error
        .unwrap()
        .contains(&format!("resent as new transfer #{new_id}")));
    let seen = perseus::seen::SeenStore::open(cfg.db_path()).unwrap();
    assert!(
        seen.source_for_package(&new_row.package_ref).unwrap().is_some(),
        "the live seen linkage follows the diverted package"
    );
    assert!(
        seen.source_for_package(&old_row.package_ref).unwrap().is_none(),
        "no live linkage remains under the declined package"
    );

    agent.shutdown().await;
}

// ---------------------------------------------------------------------------
// Task 5 (Perseus UI v2): obligation-gated source deletion + batch-history
// delete, driven against a REAL confirmed transfer through the web router.
// ---------------------------------------------------------------------------

/// Drive the web router in-process (no bound socket) with a JSON POST body.
async fn web_post(app: &axum::Router, uri: &str, body: serde_json::Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Drive the web router in-process with a GET.
async fn web_get(app: &axum::Router, uri: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(uri)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn web_body(res: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Spec §8 happy path against a REAL confirmed transfer: two fixtures land in one
/// batch, confirm, then a detached `WebState` over the same data dir runs the two
/// destructive endpoints. `POST /api/delete-files` removes the actual capture
/// files from disk and stamps the batch (the obligation is met — confirmed);
/// `POST /api/delete` history-deletes the group so `GET /api/transfers` is empty.
#[tokio::test]
async fn delete_files_and_history_after_real_confirm() {
    let tmp = tempfile::tempdir().unwrap();
    // Both fixtures pre-exist → the initial scan coalesces them into ONE batch
    // (2s quiet window covers their same-tick stabilization).
    let (agent, cfg, _stats, capture) =
        resend_rig(tmp.path(), &["frame1.fits", "frame2.fits"], 2).await;

    wait_until(
        || {
            is_confirmed(&history_for(&agent, "frame1.fits"))
                && is_confirmed(&history_for(&agent, "frame2.fits"))
        },
        WAIT,
    )
    .await;
    let rows = agent.store().all_outbound(10).unwrap();
    assert_eq!(rows.len(), 1, "both fixtures coalesced into one confirmed batch");
    let package_ref = rows[0].package_ref.clone();

    // The source capture files are still on disk (retention keeps everything).
    let src1 = capture.join("frame1.fits");
    let src2 = capture.join("frame2.fits");
    assert!(src1.exists() && src2.exists(), "sources present before delete-files");

    // Quiesce the agent, then open a fresh set of stores over the SAME perseus.db
    // and a detached WebState (the delete paths never touch the engine).
    agent.shutdown().await;
    let db = cfg.db_path();
    let store = Arc::new(StandaloneSyncStore::open(&db).unwrap());
    let seen = Arc::new(perseus::seen::SeenStore::open(&db).unwrap());
    let batches = Arc::new(perseus::batch_store::BatchStore::open(&db).unwrap());
    let (_state_tx, state_rx) = tokio::sync::watch::channel(AgentState::NeedsSetup { needs: vec![] });
    let state = Arc::new(WebState::detached(
        store,
        seen,
        batches,
        cfg.clone(),
        cfg.data_dir.join("perseus.toml"),
        state_rx,
        Arc::new(tokio::sync::Notify::new()),
    ));
    let app = build_router(Arc::clone(&state), None);

    // delete-files: the batch is confirmed (obligation met) → 200; both REAL
    // sources are removed from disk and the batch is stamped.
    let res = web_post(&app, "/api/delete-files", serde_json::json!({ "packageRef": package_ref })).await;
    assert_eq!(res.status(), axum::http::StatusCode::OK, "a confirmed batch is deletable");
    let v = web_body(res).await;
    assert_eq!(
        v["removed"].as_array().unwrap().len(),
        2,
        "both real capture sources removed: {v}"
    );
    assert!(!src1.exists() && !src2.exists(), "the capture fixtures are gone from disk");
    assert!(v["filesDeletedAt"].as_str().is_some(), "the batch was stamped: {v}");

    // history delete: the group is terminal → 200; GET /api/transfers is empty.
    let res = web_post(&app, "/api/delete", serde_json::json!({ "packageRefs": [package_ref] })).await;
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    let v = web_body(res).await;
    assert_eq!(
        v["deleted"].as_array().unwrap().len(),
        1,
        "the terminal group was history-deleted: {v}"
    );
    let transfers = web_body(web_get(&app, "/api/transfers").await).await;
    assert!(
        transfers.as_array().unwrap().is_empty(),
        "no transfers remain after history delete: {transfers}"
    );
}
