//! Acceptance floor for task A7: the four named receive-side tests.
//!
//! `ingest_lands_files_and_rows`, `duplicate_delivery_single_row_but_acked`, and
//! `primary_wins_metadata` exercise [`ingest_package`] directly against a catalog
//! connection (deterministic, no timing). `ack_replay_from_receipt_log` drives
//! the full [`SyncReceiver`] over the in-process
//! [`LoopbackTransport`](crate::sharing::loopback::LoopbackTransport): a re-sent
//! announce for a fully-receipted package must re-ack from the log without
//! re-fetching or re-ingesting.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use tempfile::TempDir;
use tokio::sync::mpsc::Receiver;
use tokio::time::Instant;

use crate::db::schema::init_db;
use crate::events::NullEmitter;
use crate::fits_writer::write_fits_f32;
use crate::models::{Frame, ImageType};
use crate::package::{self, ManifestRecord, PayloadKind, MANIFEST_VERSION};
use crate::sharing::loopback::{LoopbackNetwork, LoopbackTransport};
use crate::sharing::types::{FrameReceipt, NodeId, PackageAnnounce, ReceiptOutcome, TransportEvent};
use crate::sharing::SharingTransport;

use super::ingest::ingest_package;
use super::store::CatalogSyncStore;
use super::receiver::SyncReceiver;

const ORIGIN_DEVICE: &str = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
const PEER_HEX: &str = "1122334455667788112233445566778811223344556677881122334455667788";

/// A realistic `models::Frame` snapshot for a manifest, with an explicit uuid +
/// updated_at (the identity + primary-wins comparison anchors).
fn fixture_frame(uuid: &str, object: &str, updated_at: &str) -> Frame {
    let date_obs: DateTime<Utc> = "2026-01-15T22:30:00Z".parse().unwrap();
    Frame {
        object: Some(object.to_string()),
        date_obs: Some(date_obs),
        telescop: Some("APM107".to_string()),
        instrume: Some("ASI2600MM".to_string()),
        exptime: Some(300.0),
        filter: Some("Ha".to_string()),
        imagetyp: Some(ImageType::Light),
        gain: Some(100.0),
        offset: Some(50.0),
        binning: Some("1x1".to_string()),
        naxis1: Some(4),
        naxis2: Some(4),
        uuid: Some(uuid.to_string()),
        updated_at: Some(updated_at.to_string()),
        ..Default::default()
    }
}

/// Write a minimal real FITS file at `path` (4x4 mono, no user cards).
fn write_fits(path: &Path) {
    write_fits_f32(path, 4, 4, 1, &[0.0f32; 16], &[]).unwrap();
}

/// Build a one-frame fixture package under `root` and return `(pkg_dir, announce)`.
/// The payload is a real FITS file; the manifest carries `frame_uuid` and a
/// serialized `frame_meta` snapshot whose `object`/`updated_at` are `object` /
/// `updated_at`.
fn build_fixture_package(
    root: &Path,
    frame_uuid: &str,
    filename: &str,
    object: &str,
    updated_at: &str,
) -> (PathBuf, PackageAnnounce) {
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join(filename);
    write_fits(&src);

    let byte_size = std::fs::metadata(&src).unwrap().len();
    let xxh3 = package::xxh3_full_file(&src).unwrap();
    let frame = fixture_frame(frame_uuid, object, updated_at);
    let record = ManifestRecord {
        v: MANIFEST_VERSION,
        frame_uuid: frame_uuid.to_string(),
        origin_catalog_uuid: "catalog-uuid".to_string(),
        origin_device: ORIGIN_DEVICE.to_string(),
        payload_kind: PayloadKind::RawFrame,
        rel_path: filename.to_string(),
        byte_size,
        xxh3,
        frame_meta: serde_json::to_value(&frame).unwrap(),
        analysis: None,
        app_version: "test".to_string(),
    };

    let pkg_dir = root.join(format!("pkg-{frame_uuid}"));
    let announce = package::write_package(&pkg_dir, vec![(src, record)]).unwrap();
    (pkg_dir, announce)
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// A catalog connection with the full schema (sync tables included).
fn catalog_conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    conn
}

#[test]
fn ingest_lands_files_and_rows() {
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let (pkg_dir, announce) =
        build_fixture_package(tmp.path(), "frame-uuid-1", "L_0001.fits", "M31", "2026-01-16T10:00:00.000Z");

    let conn = catalog_conn();
    let outcome = ingest_package(&conn, &incoming, &pkg_dir, &announce, PEER_HEX).unwrap();

    // Catalog rows created from manifest metadata.
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1, "one files row");
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM frames"), 1, "one frames row");
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM fits_header"), 1, "one fits_header row");

    // frames.uuid carries the manifest frame_uuid (so a redelivery dedups).
    let uuid: String = conn
        .query_row("SELECT uuid FROM frames LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(uuid, "frame-uuid-1");
    let object: Option<String> = conn
        .query_row("SELECT object FROM frames LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(object.as_deref(), Some("M31"));

    // File landed under <incoming>/<device_short>/<date>/.
    let landed_path: String = conn
        .query_row("SELECT path FROM files LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert!(Path::new(&landed_path).exists(), "landed file exists on disk");
    assert!(landed_path.contains("incoming"), "under incoming root: {landed_path}");
    assert!(landed_path.contains("2026-01-15"), "date-bucketed by DATE-OBS: {landed_path}");

    // History + receipt written.
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM sync_history WHERE direction='received' AND outcome='ingested'"),
        1,
        "one received/ingested history row"
    );
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM sync_receipts"), 1, "one receipt row");

    // Receipt reflects an ingest.
    assert_eq!(outcome.ingested, 1);
    assert_eq!(outcome.receipts.len(), 1);
    assert!(matches!(outcome.receipts[0].outcome, ReceiptOutcome::Ingested));
}

#[test]
fn duplicate_delivery_single_row_but_acked() {
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let (pkg_dir, announce1) =
        build_fixture_package(tmp.path(), "frame-uuid-2", "L_0002.fits", "M42", "2026-01-16T10:00:00.000Z");

    let conn = catalog_conn();

    // First delivery ingests.
    let out1 = ingest_package(&conn, &incoming, &pkg_dir, &announce1, PEER_HEX).unwrap();
    assert_eq!(out1.ingested, 1);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1);

    // Second delivery of the SAME package dir but a fresh announce (the sender
    // mints a new package_id per announce) — dedup by uuid, no new catalog row,
    // receipt = Duplicate.
    let announce2 = PackageAnnounce {
        package_id: crate::sharing::types::PackageId("second-delivery".to_string()),
        ..announce1.clone()
    };
    let out2 = ingest_package(&conn, &incoming, &pkg_dir, &announce2, PEER_HEX).unwrap();

    assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1, "still one files row");
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM frames"), 1, "still one frames row");
    assert_eq!(out2.duplicate, 1, "second delivery deduped");
    assert!(matches!(out2.receipts[0].outcome, ReceiptOutcome::Duplicate), "receipt says Duplicate");
    // The ack still carries a full receipt set (one per frame) so the sender confirms.
    assert_eq!(out2.receipts.len(), 1);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM sync_history WHERE outcome='duplicate'"),
        1,
        "duplicate history recorded"
    );
}

#[test]
fn primary_wins_metadata() {
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let conn = catalog_conn();

    // Pre-seed a frame with uuid X that the primary has edited AFTER the incoming
    // snapshot (newer updated_at, edited object).
    conn.execute(
        "INSERT INTO files (path, filename, size, modified_at, format, created_at, content_hash)
         VALUES ('/local/edited.fits', 'edited.fits', 100, '2026-01-15T00:00:00Z', 'FITS', '2026-01-15T00:00:00Z', 'localhash')",
        [],
    )
    .unwrap();
    let file_id: i64 = conn.query_row("SELECT id FROM files LIMIT 1", [], |r| r.get(0)).unwrap();
    conn.execute(
        "INSERT INTO frames (file_id, object, imagetyp, uuid, updated_at)
         VALUES (?1, 'EDITED_ON_PRIMARY', 'Light', 'frame-uuid-3', '2030-01-01T00:00:00.000Z')",
        rusqlite::params![file_id],
    )
    .unwrap();

    // Deliver an OLDER snapshot for the same uuid.
    let (pkg_dir, announce) =
        build_fixture_package(tmp.path(), "frame-uuid-3", "L_0003.fits", "ORIGINAL_NAME", "2020-01-01T00:00:00.000Z");
    let out = ingest_package(&conn, &incoming, &pkg_dir, &announce, PEER_HEX).unwrap();

    // Not overwritten: still one frame, object unchanged, receipt Duplicate.
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM frames"), 1, "no new frame inserted");
    let object: String = conn
        .query_row("SELECT object FROM frames WHERE uuid='frame-uuid-3'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(object, "EDITED_ON_PRIMARY", "primary edit preserved");
    assert_eq!(out.skipped_older, 1, "counted as skipped_older");
    assert!(matches!(out.receipts[0].outcome, ReceiptOutcome::Duplicate));
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM sync_history WHERE outcome='skipped_older'"),
        1,
        "history notes skipped_older"
    );
}

// ── ack_replay: full receiver over LoopbackTransport ────────────────────────

/// Wait for the next `AckReceived` on `events` matching `package_id`.
async fn wait_for_ack(
    events: &mut Receiver<TransportEvent>,
    package_id: &str,
    timeout: Duration,
) -> Vec<FrameReceipt> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let ev = tokio::time::timeout(remaining, events.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for ack of {package_id}"))
            .expect("sender event stream closed");
        if let TransportEvent::AckReceived { package_id: id, receipts, .. } = ev {
            if id.0 == package_id {
                return receipts;
            }
        }
    }
}

#[tokio::test]
async fn ack_replay_from_receipt_log() {
    let tmp = TempDir::new().unwrap();
    let catalog_path = tmp.path().join("catalog.db");
    // Initialise the catalog schema (and hold a connection open for assertions).
    let assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();

    let sync_dir = tmp.path().join("sync");
    let incoming = sync_dir.join("incoming");
    let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

    // Loopback network: sender + receiver endpoints.
    let net = LoopbackNetwork::new();
    let sender: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_ep: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let receiver_node: NodeId = receiver_ep.node_id();

    // Bring the sender online and take its ack stream.
    sender.start().await.unwrap();
    let mut sender_events = sender.events().await;

    // Spawn the real receiver over its endpoint.
    let (_info, _handle) = SyncReceiver::spawn(
        Arc::clone(&store),
        incoming.clone(),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .unwrap();

    // Build + serve a fixture package.
    let (pkg_dir, announce) =
        build_fixture_package(tmp.path(), "frame-uuid-4", "L_0004.fits", "NGC7000", "2026-01-16T10:00:00.000Z");
    sender.serve(&announce, &pkg_dir).await.unwrap();

    // First delivery: announce → receiver fetches, ingests, acks.
    sender.announce(receiver_node, &announce).await.unwrap();
    let receipts1 = wait_for_ack(&mut sender_events, &announce.package_id.0, Duration::from_secs(5)).await;
    assert_eq!(receipts1.len(), 1);
    assert!(matches!(receipts1[0].outcome, ReceiptOutcome::Ingested));

    // The first ingest landed exactly one file/frame and wrote one history +
    // one receipt row.
    {
        let c = assert_db.conn();
        assert_eq!(count(&c, "SELECT COUNT(*) FROM files"), 1, "first delivery ingested one file");
        assert_eq!(count(&c, "SELECT COUNT(*) FROM sync_history"), 1, "one history row after first");
        assert_eq!(count(&c, "SELECT COUNT(*) FROM sync_receipts"), 1, "one receipt row after first");
    }

    // Second delivery of the SAME announce (same package_id): the receiver must
    // re-ack from the receipt log WITHOUT re-fetching or re-ingesting.
    sender.announce(receiver_node, &announce).await.unwrap();
    let receipts2 = wait_for_ack(&mut sender_events, &announce.package_id.0, Duration::from_secs(5)).await;

    // Identical receipts, replayed straight from the log.
    assert_eq!(receipts2.len(), 1);
    assert_eq!(receipts2[0].frame_uuid, receipts1[0].frame_uuid);
    assert_eq!(receipts2[0].xxh3, receipts1[0].xxh3);

    // No re-ingest: file/frame count unchanged AND the replay wrote no new
    // history or receipt rows (replay short-circuits before ingest).
    {
        let c = assert_db.conn();
        assert_eq!(count(&c, "SELECT COUNT(*) FROM files"), 1, "replay did not re-ingest a file");
        assert_eq!(count(&c, "SELECT COUNT(*) FROM sync_history"), 1, "replay wrote no history row");
        assert_eq!(count(&c, "SELECT COUNT(*) FROM sync_receipts"), 1, "replay wrote no receipt row");
    }
}
