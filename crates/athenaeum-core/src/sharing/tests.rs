//! Acceptance tests for the loopback transport (task A2 floor).
//!
//! These are the three named tests from the brief. They drive the
//! [`SharingTransport`] surface end-to-end over the in-process
//! [`LoopbackNetwork`] and exercise the fault knobs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::tempdir;
use tokio::sync::mpsc::Receiver;

use crate::package::{self, write_package, ManifestRecord, PayloadKind, MANIFEST_VERSION};
use super::loopback::{FaultPlan, LoopbackNetwork};
use super::types::{
    FetchEvent, FrameReceipt, PackageAnnounce, PackageId, ReceiptOutcome, TransportEvent,
};
use super::{noop_fetch_sink, FetchSink, SharingTransport};

/// A [`FetchSink`] that appends every event into a shared vec, plus the vec so a
/// test can inspect the collected series after the fetch.
fn recording_sink() -> (FetchSink, Arc<Mutex<Vec<FetchEvent>>>) {
    let events: Arc<Mutex<Vec<FetchEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_events = Arc::clone(&events);
    let sink: FetchSink = Arc::new(move |ev| {
        sink_events.lock().expect("sink mutex poisoned").push(ev);
    });
    (sink, events)
}

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

    provider.serve(&pkg, src.path(), None).await.unwrap();
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
        .fetch(provider_info.node_id, &pkg, dest.path(), noop_fetch_sink())
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

    // The receiver's `fetch` above pushed synthetic `ServeProgress` +
    // `ServeFileProgress` + `ServeComplete` onto the provider's event stream (Task
    // 13 / 2.1 / 2.2); skip those best-effort local signals to reach the ack this
    // test asserts on.
    let ack = loop {
        match recv_next(&mut provider_events).await {
            TransportEvent::ServeProgress { .. }
            | TransportEvent::ServeFileProgress { .. }
            | TransportEvent::ServeComplete { .. } => continue,
            ev => break ev,
        }
    };
    match ack {
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

/// After `release`, the provider stops serving the package: a subsequent fetch
/// of the same package fails with a "not served" error. Releasing again (or an
/// unknown id) is idempotent and Ok.
#[tokio::test]
async fn loopback_release_makes_package_unfetchable() {
    let net = LoopbackNetwork::new();
    let provider = net.endpoint();
    let receiver = net.endpoint();

    let provider_info = provider.start().await.unwrap();
    receiver.start().await.unwrap();

    let src = tempdir().unwrap();
    write_blob(src.path(), "frame_0001.fits", 64 * 1024);
    let pkg = sample_announce();
    provider.serve(&pkg, src.path(), None).await.unwrap();

    // Served → fetch succeeds.
    let dest1 = tempdir().unwrap();
    receiver
        .fetch(provider_info.node_id, &pkg, dest1.path(), noop_fetch_sink())
        .await
        .unwrap();

    // Released → the same fetch now fails with "not served".
    provider.release(&pkg.package_id).await.unwrap();
    let dest2 = tempdir().unwrap();
    let err = receiver
        .fetch(provider_info.node_id, &pkg, dest2.path(), noop_fetch_sink())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not served"), "got: {err}");

    // Idempotent: releasing again (or an unknown id) is Ok.
    provider.release(&pkg.package_id).await.unwrap();
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
    provider.serve(&pkg, src.path(), None).await.unwrap();

    // Arm the one-shot fault: abort after 32 KiB copied.
    receiver.set_fault(FaultPlan {
        abort_after_bytes: Some(32 * 1024),
        ..Default::default()
    });

    let dest = tempdir().unwrap();
    let first = receiver
        .fetch(provider_info.node_id, &pkg, dest.path(), noop_fetch_sink())
        .await;
    assert!(first.is_err(), "first fetch should abort mid-copy");

    // Fault is one-shot: second fetch completes and verifies.
    let second = receiver
        .fetch(provider_info.node_id, &pkg, dest.path(), noop_fetch_sink())
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

/// Build a 3-frame package (`frame1/2/3.fits` + manifest) on disk and return
/// `(pkg_dir, announce, [rel_path…])`. Each payload has distinct content so a
/// wrong-frame copy would hash-mismatch.
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

/// A want-subset serve must transfer ONLY the negotiated frames: fetch lands
/// frame1 + frame3 (not frame2) and the fetched manifest holds exactly those
/// two records. The mock's honest mirror of the iroh subset collection.
#[tokio::test]
async fn subset_serve_transfers_only_want_frames() {
    let net = LoopbackNetwork::new();
    let provider = net.endpoint();
    let receiver = net.endpoint();
    provider.start().await.unwrap();
    receiver.start().await.unwrap();

    let tmp = tempdir().unwrap();
    let (pkg_dir, announce, rels) = build_three_frame_package(&tmp.path().join("src"));

    // Want only frame1 + frame3 (drop frame2 as an already-had duplicate).
    let want: HashSet<String> = [rels[0].clone(), rels[2].clone()].into_iter().collect();
    provider
        .serve(&announce, &pkg_dir, Some(&want))
        .await
        .unwrap();

    let dest = tempdir().unwrap();
    receiver
        .fetch(provider.node_id(), &announce, dest.path(), noop_fetch_sink())
        .await
        .unwrap();

    assert!(dest.path().join(&rels[0]).exists(), "frame1 (wanted) present");
    assert!(
        !dest.path().join(&rels[1]).exists(),
        "frame2 (not wanted) must be absent"
    );
    assert!(dest.path().join(&rels[2]).exists(), "frame3 (wanted) present");

    // The fetched manifest holds exactly the two wanted records.
    let fetched = package::read_manifest(dest.path()).unwrap();
    assert_eq!(fetched.len(), 2, "filtered manifest must hold 2 records");
    let got: HashSet<String> = fetched.iter().map(|r| r.rel_path.clone()).collect();
    assert_eq!(got, want, "manifest records must be exactly the wanted rel_paths");

    // The wanted payloads round-trip byte-for-byte.
    assert_eq!(
        xxh3_of(&pkg_dir.join(&rels[0])),
        xxh3_of(&dest.path().join(&rels[0])),
        "frame1 content mismatch"
    );
}

/// The [`FetchSink`] threaded into `fetch` must report monotonic per-file
/// progress and an aggregate batch that reaches the announce's `byte_size`. This
/// pins the [`FetchEvent`] contract deterministically over the loopback: for each
/// file a `File{done:0}` then `File{done:total}`, then a final
/// `Batch{done:byte_size, total:byte_size}`.
#[tokio::test]
async fn loopback_fetch_reports_monotonic_per_file_progress() {
    let net = LoopbackNetwork::new();
    let provider = net.endpoint();
    let receiver = net.endpoint();
    provider.start().await.unwrap();
    receiver.start().await.unwrap();

    let tmp = tempdir().unwrap();
    let (pkg_dir, announce, _rels) = build_three_frame_package(&tmp.path().join("src"));
    provider.serve(&announce, &pkg_dir, None).await.unwrap();

    let (sink, events) = recording_sink();
    let dest = tempdir().unwrap();
    receiver
        .fetch(provider.node_id(), &announce, dest.path(), sink)
        .await
        .unwrap();

    let events = events.lock().unwrap().clone();

    // Every File series (grouped by name) is non-decreasing and ends complete.
    let mut per_file: std::collections::HashMap<String, Vec<(u64, u64)>> =
        std::collections::HashMap::new();
    for ev in &events {
        if let FetchEvent::File { name, bytes_done, bytes_total } = ev {
            per_file
                .entry(name.clone())
                .or_default()
                .push((*bytes_done, *bytes_total));
        }
    }
    assert!(!per_file.is_empty(), "expected at least one File event");
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

    // At least one Batch event ends at the announce's byte_size.
    let reached_total = events.iter().any(|ev| {
        matches!(
            ev,
            FetchEvent::Batch { bytes_done, bytes_total }
                if *bytes_done == announce.byte_size && *bytes_total == announce.byte_size
        )
    });
    assert!(
        reached_total,
        "expected a Batch event reaching byte_size={}, got {events:?}",
        announce.byte_size
    );
}
