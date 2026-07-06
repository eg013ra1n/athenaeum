//! Acceptance tests for the loopback transport (task A2 floor).
//!
//! These are the three named tests from the brief. They drive the
//! [`SharingTransport`] surface end-to-end over the in-process
//! [`LoopbackNetwork`] and exercise the fault knobs.

use std::path::Path;
use std::time::Duration;

use tempfile::tempdir;
use tokio::sync::mpsc::Receiver;

use super::loopback::{FaultPlan, LoopbackNetwork};
use super::types::{
    FrameReceipt, PackageAnnounce, PackageId, ReceiptOutcome, TransportEvent,
};
use super::SharingTransport;

/// Write a deterministic `size`-byte file into `dir` and return its path.
fn write_blob(dir: &Path, name: &str, size: usize) -> std::path::PathBuf {
    let path = dir.join(name);
    let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &bytes).unwrap();
    path
}

fn xxh3_of(path: &Path) -> u64 {
    let bytes = std::fs::read(path).unwrap();
    xxhash_rust::xxh3::xxh3_64(&bytes)
}

fn sample_announce() -> PackageAnnounce {
    PackageAnnounce {
        package_id: PackageId(uuid::Uuid::new_v4().to_string()),
        root_hash: "deadbeef".to_string(),
        byte_size: 0,
        frame_count: 1,
    }
}

/// Receive the next event within a timeout, failing loudly on stall.
async fn recv_next(rx: &mut Receiver<TransportEvent>) -> TransportEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("event channel stalled")
        .expect("event channel closed unexpectedly")
}

/// Provider A serves a fixture dir and announces to B; B fetches and acks; A
/// observes the receipts. The full happy-path round trip.
#[tokio::test]
async fn loopback_announce_fetch_ack_roundtrip() {
    let net = LoopbackNetwork::new();
    let provider = net.endpoint();
    let receiver = net.endpoint();

    let provider_info = provider.start().await.unwrap();
    let receiver_info = receiver.start().await.unwrap();

    let mut provider_events = provider.events().await;
    let mut receiver_events = receiver.events().await;

    // Provider stages a package on disk.
    let src = tempdir().unwrap();
    let blob = write_blob(src.path(), "frame_0001.fits", 64 * 1024);
    let pkg = sample_announce();

    provider.serve(&pkg, src.path()).await.unwrap();
    provider
        .announce(receiver_info.node_id, &pkg)
        .await
        .unwrap();

    // Receiver sees the announcement.
    match recv_next(&mut receiver_events).await {
        TransportEvent::AnnounceReceived { from, announce } => {
            assert_eq!(from, provider_info.node_id);
            assert_eq!(announce.package_id, pkg.package_id);
        }
        other => panic!("expected AnnounceReceived, got {other:?}"),
    }

    // Receiver fetches into its own dir and verifies content.
    let dest = tempdir().unwrap();
    receiver
        .fetch(provider_info.node_id, &pkg, dest.path())
        .await
        .unwrap();
    let fetched = dest.path().join("frame_0001.fits");
    assert!(fetched.exists(), "fetched blob missing");
    assert_eq!(xxh3_of(&blob), xxh3_of(&fetched), "content mismatch");

    // Receiver acks; provider observes the receipts.
    let receipts = vec![FrameReceipt {
        frame_uuid: "frame-uuid-1".to_string(),
        xxh3: format!("{:016x}", xxh3_of(&fetched)),
        outcome: ReceiptOutcome::Ingested,
    }];
    receiver
        .ack(provider_info.node_id, &pkg.package_id, receipts.clone())
        .await
        .unwrap();

    match recv_next(&mut provider_events).await {
        TransportEvent::AckReceived {
            from,
            package_id,
            receipts: got,
        } => {
            assert_eq!(from, receiver_info.node_id);
            assert_eq!(package_id, pkg.package_id);
            assert_eq!(got, receipts);
        }
        other => panic!("expected AckReceived, got {other:?}"),
    }
}

/// With `abort_after_bytes` armed the first fetch fails mid-copy; the second
/// (fault consumed) completes and the copied file hash-verifies against source.
#[tokio::test]
async fn loopback_fault_abort_mid_fetch_then_resume() {
    let net = LoopbackNetwork::new();
    let provider = net.endpoint();
    let receiver = net.endpoint();

    let provider_info = provider.start().await.unwrap();
    receiver.start().await.unwrap();

    let src = tempdir().unwrap();
    let blob = write_blob(src.path(), "frame_0001.fits", 256 * 1024);
    let pkg = sample_announce();
    provider.serve(&pkg, src.path()).await.unwrap();

    // Arm the one-shot fault: abort after 32 KiB copied.
    receiver.set_fault(FaultPlan {
        abort_after_bytes: Some(32 * 1024),
        ..Default::default()
    });

    let dest = tempdir().unwrap();
    let first = receiver
        .fetch(provider_info.node_id, &pkg, dest.path())
        .await;
    assert!(first.is_err(), "first fetch should abort mid-copy");

    // Fault is one-shot: second fetch completes and verifies.
    let second = receiver
        .fetch(provider_info.node_id, &pkg, dest.path())
        .await;
    assert!(second.is_ok(), "second fetch should succeed: {second:?}");

    let fetched = dest.path().join("frame_0001.fits");
    assert_eq!(
        xxh3_of(&blob),
        xxh3_of(&fetched),
        "resumed content must match source"
    );
}

/// `duplicate_ack` delivers the ack event twice. The transport just delivers;
/// idempotence is the engine's job — here we assert both arrivals are observable
/// and nothing panics.
#[tokio::test]
async fn loopback_duplicate_ack_delivered_once_ok() {
    let net = LoopbackNetwork::new();
    let provider = net.endpoint();
    let receiver = net.endpoint();

    let provider_info = provider.start().await.unwrap();
    let receiver_info = receiver.start().await.unwrap();
    let mut provider_events = provider.events().await;

    let pkg = sample_announce();
    let receipts = vec![FrameReceipt {
        frame_uuid: "frame-uuid-1".to_string(),
        xxh3: "0000000000000000".to_string(),
        outcome: ReceiptOutcome::Duplicate,
    }];

    receiver.set_fault(FaultPlan {
        duplicate_ack: true,
        ..Default::default()
    });
    receiver
        .ack(provider_info.node_id, &pkg.package_id, receipts.clone())
        .await
        .unwrap();

    // Two identical AckReceived events must arrive.
    for _ in 0..2 {
        match recv_next(&mut provider_events).await {
            TransportEvent::AckReceived {
                from,
                package_id,
                receipts: got,
            } => {
                assert_eq!(from, receiver_info.node_id);
                assert_eq!(package_id, pkg.package_id);
                assert_eq!(got, receipts);
            }
            other => panic!("expected AckReceived, got {other:?}"),
        }
    }
}
